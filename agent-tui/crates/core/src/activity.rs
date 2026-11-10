//! What each agent is doing, as its harness reports it through hooks
//! (`agent event`), so the TUI can show it and say which agents need you.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::ports::Probe;

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

/// Where a harness keeps its record of the conversation, as it names it. Only
/// the harness that produced it knows how to read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TranscriptRef(String);

impl TranscriptRef {
    pub fn new(locator: impl Into<String>) -> Self {
        Self(locator.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The locator as a file, for the harnesses that keep transcripts in
    /// files, which is all of them so far.
    pub fn path(&self) -> &std::path::Path {
        std::path::Path::new(&self.0)
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

/// Where a reporting process came from, as far as it could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lineage {
    /// Its ancestry wasn't read, or couldn't be.
    Untraced,
    /// The process and its ancestors, nearest first. A process with no
    /// ancestors to speak of has a chain of just itself.
    Traced(Vec<Pid>),
}

/// An event and who it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    pub event: Event,
    pub lineage: Lineage,
    /// Where the harness said the agent's transcript is, when the event
    /// carried it.
    pub transcript: Option<TranscriptRef>,
    pub interrupts: InterruptSignal,
}

/// How the harness lets the TUI know the user interrupted an activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptSignal {
    /// It sends an event, so the TUI has nothing to look for.
    #[default]
    Reported,
    /// Only the transcript shows it, which the TUI has to read.
    Transcript,
}

impl Reported {
    pub fn main(event: impl Into<MainEvent>) -> Self {
        Self {
            event: Event::Main(event.into()),
            lineage: Lineage::Untraced,
            transcript: None,
            interrupts: InterruptSignal::Reported,
        }
    }

    pub fn subagent(id: AgentId, event: impl Into<SubagentEvent>) -> Self {
        Self {
            event: Event::Subagent(id, event.into()),
            lineage: Lineage::Untraced,
            transcript: None,
            interrupts: InterruptSignal::Reported,
        }
    }

    #[must_use]
    pub fn interrupted_in_transcript(mut self) -> Self {
        self.interrupts = InterruptSignal::Transcript;
        self
    }

    #[must_use]
    pub fn at_transcript(mut self, transcript: Option<TranscriptRef>) -> Self {
        self.transcript = transcript;
        self
    }

    #[must_use]
    pub fn from_lineage(mut self, lineage: Lineage) -> Self {
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
    /// The activity ended. `summary` is the agent's last message, and `running`
    /// the background subagents it's still waiting on.
    Finished {
        summary: Option<String>,
        running: Vec<Background>,
    },
    /// The activity ended in an error (rate limited, overloaded…).
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

/// A background subagent a finished activity is still waiting on.
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
    /// The activity ended but background subagents are still running, so
    /// another activity follows when they report back.
    Delegating,
    /// Finished an activity the user hasn't looked at yet.
    Done {
        summary: Option<String>,
    },
    Failed {
        reason: String,
    },
    /// The user stopped the activity. It stays until the next prompt.
    Interrupted,
    Exited,
}

impl State {
    /// Working or delegating: the activity has more to do.
    pub fn is_going(&self) -> bool {
        matches!(self, Self::Working { .. } | Self::Delegating)
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(into = "WrittenSubagent")]
pub struct Subagent {
    pub kind: String,
    pub description: Option<String>,
    pub launch: Launch,
    pub since: SystemTime,
    /// When the harness last reported anything from it, or about it while
    /// it was listed as running.
    heard: SystemTime,
    status: SubagentStatus,
}

impl Subagent {
    /// A subagent that began at `since` and is running.
    pub fn running(
        kind: impl Into<String>,
        description: Option<String>,
        launch: Launch,
        since: SystemTime,
    ) -> Self {
        Self {
            kind: kind.into(),
            description,
            launch,
            since,
            heard: since,
            status: SubagentStatus::Running { tools: Vec::new() },
        }
    }

    /// Its description, unless it is missing or empty.
    pub fn label(&self) -> Option<&str> {
        self.description.as_deref().filter(|d| !d.is_empty())
    }

    /// What names it in a row: its description, else its kind.
    pub fn title(&self) -> &str {
        self.label().unwrap_or(&self.kind)
    }
}

/// Whether a subagent is still going. A finished one is kept until the next
/// prompt so its row and transcript stay to be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SubagentStatus {
    /// The tools it's running.
    Running {
        tools: Vec<Tool>,
    },
    Finished {
        at: SystemTime,
    },
}

/// How a subagent stands as of a moment, which is its recorded status
/// unless it has gone quiet. Nothing in the record says a subagent that
/// reports nothing is still alive: its session may have died with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness<'a> {
    Running {
        tools: &'a [Tool],
    },
    /// Recorded as running, with no tool running, no question open and
    /// nothing heard from it for [`STALE_AFTER`].
    Lost {
        since: SystemTime,
    },
    Finished {
        at: SystemTime,
    },
}

/// Whether a subagent has a question open. It is then waiting on the user,
/// not gone quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    On,
    Off,
}

/// Whether nothing has been heard since `heard` for longer than
/// [`STALE_AFTER`].
fn quiet(heard: SystemTime, now: SystemTime) -> bool {
    now.duration_since(heard).is_ok_and(|d| d > STALE_AFTER)
}

impl Subagent {
    pub fn liveness(&self, now: SystemTime, waiting: Waiting) -> Liveness<'_> {
        match (&self.status, waiting) {
            (SubagentStatus::Finished { at }, _) => Liveness::Finished { at: *at },
            (SubagentStatus::Running { tools }, Waiting::Off)
                if tools.is_empty() && quiet(self.heard, now) =>
            {
                Liveness::Lost { since: self.heard }
            }
            (SubagentStatus::Running { tools }, _) => Liveness::Running { tools },
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.status, SubagentStatus::Finished { .. })
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
        if !self.is_finished() {
            self.status = SubagentStatus::Finished { at };
        }
    }

    fn start(&mut self, tool: Tool) {
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

/// How many reports an activity had taken when a proof was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Revision(u64);

/// Proof that an activity was going when its transcript was about to be
/// read, and as of which report. Only [`Activity::going`] makes one, and
/// only an activity still at that report takes what the read concluded, so
/// a conclusion about an earlier turn can't change a later one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Going {
    revision: Revision,
    after: SystemTime,
}

/// The same for an activity that was interrupted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped {
    revision: Revision,
    after: SystemTime,
}

/// What a transcript is read to find out about an activity, with the proof
/// the answer is applied under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Whether the user has interrupted it.
    Going(Going),
    /// Whether it has carried on after all.
    Stopped(Stopped),
}

/// What an answer about a transcript does to the activity it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// The user interrupted it, by the record written at the time given.
    Interrupted(Going, SystemTime),
    Resumed(Stopped),
}

impl Standing {
    /// Records older than this say nothing about the activity as it stands.
    pub fn after(self) -> SystemTime {
        match self {
            Self::Going(Going { after, .. }) | Self::Stopped(Stopped { after, .. }) => after,
        }
    }

    /// How `probe` moves the activity this proves, if it does rather than
    /// confirms it.
    pub fn moved_by(self, probe: &Probe) -> Option<Move> {
        match (self, probe) {
            (Self::Going(proof), Probe::Interrupted(_, at)) => Some(Move::Interrupted(proof, *at)),
            (Self::Stopped(proof), Probe::Going(_)) => Some(Move::Resumed(proof)),
            _ => None,
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
    /// How many prompts from the user the agent has taken, which tells one
    /// activity from the next whatever the words were.
    pub prompts: u64,
    /// The count of events that included the last prompt of the user's, so
    /// what was reported after it can be told from what came with it.
    pub prompt_event: u64,
    /// When the harness last reported anything.
    pub heard: SystemTime,
    /// Counts the harness's reports and the conclusions applied under a proof,
    /// so a view can show progress as it happens and a proof can tell
    /// whether anything has been applied since it was made.
    pub events: u64,
    /// Open questions, at most one per asker, oldest first.
    pub asks: Vec<Ask>,
    pub subagents: BTreeMap<AgentId, Subagent>,
    /// Subagents that stopped before anything said they began, when they
    /// stopped. Never drawn or counted, so a stop for one the activity
    /// dropped can't show as a phantom. A later start or launch takes its
    /// stop from here.
    stopped_early: BTreeMap<AgentId, SystemTime>,
    pub session: Session,
    /// The latest transcript the harness reported.
    pub transcript: Option<TranscriptRef>,
    pub interrupts: InterruptSignal,
}

/// How long a launch that has reported nothing holds typed text. A session
/// that never started (the host or the terminal failed after the launch was
/// recorded) leaves no later report, so without a bound the agent would
/// refuse text for good.
pub const LAUNCH_GRACE: Duration = Duration::from_secs(30);

/// How long a session that just reported starting must stay quiet before
/// typed text is safe: a startup dialog may be open without any report.
pub const BOOT_QUIET: Duration = Duration::from_secs(5);

/// How a session that is still coming up got there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// Launched through `agent` and nothing reported since.
    Launching { since: SystemTime },
    /// The harness reported starting, and a dialog may still be open.
    Starting { since: SystemTime },
}

impl Boot {
    pub fn since(self) -> SystemTime {
        match self {
            Self::Launching { since } | Self::Starting { since } => since,
        }
    }
}

/// Why text can't be typed into an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// A question or permission menu is open, where typed text could pick
    /// an option.
    Asking,
    /// The session only just reported starting, and a startup dialog may be
    /// open without any report.
    Booting,
    /// The harness reported the session over, so what's typed would reach
    /// whatever is left in its terminal.
    Ended,
}

impl Hold {
    /// What is wrong with typing into `agent`, as a sentence.
    pub fn says(self, agent: &impl std::fmt::Display) -> String {
        match self {
            Self::Asking => format!("{agent} is asking"),
            Self::Booting => format!("{agent} is still booting"),
            Self::Ended => format!("{agent} has ended"),
        }
    }

    /// The reason alone, for a title.
    pub fn label(self) -> &'static str {
        match self {
            Self::Asking => "asking",
            Self::Booting => "booting",
            Self::Ended => "ended",
        }
    }

    /// What to do about it.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Asking => "attach to answer",
            Self::Booting => "try again in a moment",
            Self::Ended => "attach to restart it",
        }
    }
}

impl Activity {
    /// Whether the user's prompt began an activity since `before`. A subagent
    /// handing its result back to an activity that is still going, or the same
    /// session starting again, takes no prompt of the user's.
    pub fn has_new_prompt_since(&self, before: &Self) -> bool {
        self.prompts != before.prompts
    }

    /// Whether the user has given the agent a prompt.
    pub fn has_taken_a_prompt(&self) -> bool {
        self.prompts > 0
    }

    /// What an agent that has reported nothing yet is taken to be doing.
    pub fn fresh(now: SystemTime) -> Self {
        Self {
            state: State::Working { tools: Vec::new() },
            since: now,
            prompt: None,
            prompts: 0,
            prompt_event: 0,
            heard: now,
            events: 0,
            asks: Vec::new(),
            subagents: BTreeMap::new(),
            stopped_early: BTreeMap::new(),
            session: Session::Live {
                join: Join::Untraced,
            },
            transcript: None,
            interrupts: InterruptSignal::Reported,
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
        if reported.transcript.is_some() {
            activity.transcript = reported.transcript;
        }
        activity.interrupts = reported.interrupts;
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
            (_, _, Event::Main(MainEvent::Launched(_)))
            | (Join::Untraced, _, _)
            | (_, Lineage::Untraced, _) => false,
            (Join::Fresh(pid), Lineage::Traced(lineage), _) => !lineage.contains(&pid),
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
            Event::Subagent(id, event) => self.apply_subagent(&id, event, state, answered, now),
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
            MainEvent::Launched(_) | MainEvent::Started => {
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
                    self.prompts += 1;
                    self.prompt_event = self.events + 1;
                    self.keep_background();
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
        id: &AgentId,
        event: SubagentEvent,
        state: State,
        answered: Option<Ask>,
        now: SystemTime,
    ) -> State {
        let state = self.apply_subagent_event(id.clone(), event, state, answered, now);
        if let Some(subagent) = self.subagents.get_mut(id) {
            subagent.heard = now;
        }
        state
    }

    /// The subagent `id`, listed if it wasn't: finished already when its
    /// stop came first.
    fn enter(
        &mut self,
        id: AgentId,
        kind: String,
        launch: Launch,
        now: SystemTime,
    ) -> &mut Subagent {
        let stopped = self.stopped_early.remove(&id);
        self.subagents.entry(id).or_insert_with(|| {
            let mut subagent = Subagent::running(kind, None, launch, now);
            if let Some(at) = stopped {
                subagent.finish(at);
            }
            subagent
        })
    }

    fn apply_subagent_event(
        &mut self,
        id: AgentId,
        event: SubagentEvent,
        state: State,
        answered: Option<Ask>,
        now: SystemTime,
    ) -> State {
        match event {
            SubagentEvent::Started { kind } => {
                self.enter(id, kind, Launch::Foreground, now);
                state
            }
            SubagentEvent::Launched {
                tool,
                kind,
                description,
            } => {
                let subagent = self.enter(id, kind, Launch::Background, now);
                subagent.launch = Launch::Background;
                subagent.description = Some(description);
                finish(state, &tool)
            }
            SubagentEvent::Stopped => {
                match self.subagents.get_mut(&id) {
                    Some(subagent) => subagent.finish(now),
                    None => {
                        self.stopped_early.insert(id, now);
                    }
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

    /// The finished activity's background subagents replace the ones tracked.
    fn track_background(&mut self, running: &[Background], now: SystemTime) {
        self.subagents
            .retain(|id, s| s.is_finished() || running.iter().any(|b| &b.id == id));
        for b in running
            .iter()
            .filter(|b| !self.stopped_early.contains_key(&b.id))
        {
            let subagent = self.subagents.entry(b.id.clone()).or_insert_with(|| {
                Subagent::running(b.kind.clone(), None, Launch::Background, now)
            });
            subagent.launch = Launch::Background;
            subagent.heard = now;
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

    /// Drops what only the foreground held: its subagents, and every question
    /// except those of the background subagents still running.
    fn keep_background(&mut self) {
        self.subagents
            .retain(|_, s| s.launch == Launch::Background && !s.is_finished());
        self.stopped_early.clear();
        let subagents = &self.subagents;
        self.asks.retain(|ask| match &ask.asker {
            Origin::Subagent(id) => subagents.contains_key(id),
            Origin::Main => false,
        });
    }

    fn forget_subagents(&mut self) {
        self.subagents.clear();
        self.stopped_early.clear();
        self.asks.clear();
    }

    /// Why typed text could land in a menu or nowhere, if it could. A
    /// question that is open takes it as an answer, and so does a startup
    /// dialog a session that only just reported starting may still show.
    /// Working isn't a reason: the harness queues what is typed
    /// mid-activity. A permission prompt not reported yet, and a menu a
    /// `/` or `!` command opens, are not detected.
    pub fn hold(&self, now: SystemTime) -> Option<Hold> {
        if matches!(self.state, State::Exited) {
            return Some(Hold::Ended);
        }
        if !self.asks.is_empty() {
            return Some(Hold::Asking);
        }
        self.boot(now).map(|_| Hold::Booting)
    }

    /// Whether the session is still coming up, which the card shows as
    /// starting and typed text is refused for. A launch that reports nothing
    /// for [`LAUNCH_GRACE`] is taken to have failed, and a session that just
    /// reported starting is quiet for [`BOOT_QUIET`] for a startup dialog.
    pub fn boot(&self, now: SystemTime) -> Option<Boot> {
        match self.session {
            Session::Launched { at, .. }
                if now
                    .duration_since(at)
                    .is_ok_and(|waited| waited < LAUNCH_GRACE) =>
            {
                Some(Boot::Launching { since: at })
            }
            Session::Booting { .. }
                if now
                    .duration_since(self.heard)
                    .is_ok_and(|quiet| quiet < BOOT_QUIET) =>
            {
                Some(Boot::Starting { since: self.heard })
            }
            Session::Launched { .. }
            | Session::LaunchReported { .. }
            | Session::Booting { .. }
            | Session::Live { .. } => None,
        }
    }

    /// How subagent `id` stands as of `now`. One with a question open is
    /// waiting on the user, not lost.
    pub fn liveness(&self, id: &AgentId, now: SystemTime) -> Option<Liveness<'_>> {
        let waiting = if self
            .asks
            .iter()
            .any(|ask| matches!(&ask.asker, Origin::Subagent(asker) if asker == id))
        {
            Waiting::On
        } else {
            Waiting::Off
        };
        Some(self.subagents.get(id)?.liveness(now, waiting))
    }

    fn count(&self, now: SystemTime, wanted: impl Fn(Liveness<'_>) -> bool) -> usize {
        self.subagents
            .keys()
            .filter_map(|id| self.liveness(id, now))
            .filter(|l| wanted(*l))
            .count()
    }

    /// How many subagents are running as of `now`.
    pub fn running(&self, now: SystemTime) -> usize {
        self.count(now, |l| matches!(l, Liveness::Running { .. }))
    }

    /// How many subagents are recorded as running but have gone quiet.
    pub fn lost(&self, now: SystemTime) -> usize {
        self.count(now, |l| matches!(l, Liveness::Lost { .. }))
    }

    /// The question that's waited longest, if any.
    pub fn waiting(&self) -> Option<&Ask> {
        self.asks.first()
    }

    /// The user has looked at it: a finished activity stops being news. This
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

    /// The proof to read its transcript for an interrupt, while it is going.
    pub fn going(&self) -> Option<Going> {
        self.state.is_going().then_some(Going {
            revision: Revision(self.events),
            after: self.since,
        })
    }

    /// The proof to read its transcript for it carrying on, while it is
    /// interrupted.
    pub fn stopped(&self) -> Option<Stopped> {
        matches!(self.state, State::Interrupted).then_some(Stopped {
            revision: Revision(self.events),
            after: self.since,
        })
    }

    /// What its transcript is worth reading for, if anything.
    pub fn standing(&self) -> Option<Standing> {
        self.going()
            .map(Standing::Going)
            .or_else(|| self.stopped().map(Standing::Stopped))
    }

    /// The user interrupted the activity `proof` was made for, by the record
    /// written at `at`, which is when it counts as interrupted. Reports and
    /// applied conclusions are counted, so an activity still at the proof's
    /// count has had none since, and nothing else moves a going activity on.
    /// Any other activity is returned as it was.
    #[must_use]
    pub fn interrupted(mut self, proof: Going, at: SystemTime) -> Self {
        if proof.revision != Revision(self.events) {
            return self;
        }
        self.keep_background();
        self.state = State::Interrupted;
        self.since = at;
        self.events += 1;
        self
    }

    /// The interrupted activity `proof` was made for carried on after all,
    /// under the same rule as [`Self::interrupted`].
    #[must_use]
    pub fn resumed(mut self, proof: Stopped, now: SystemTime) -> Self {
        if proof.revision != Revision(self.events) {
            return self;
        }
        self.state = State::Working { tools: Vec::new() };
        self.since = now;
        self.events += 1;
        self
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
        let quiet = quiet(self.heard, now);
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
    #[serde(default)]
    prompts: u64,
    #[serde(default)]
    prompt_event: u64,
    heard: SystemTime,
    events: u64,
    #[serde(default)]
    asks: Vec<Ask>,
    #[serde(default)]
    subagents: BTreeMap<AgentId, StoredSubagent>,
    #[serde(default)]
    stopped_early: BTreeMap<AgentId, SystemTime>,
    #[serde(default)]
    session: Option<Session>,
    #[serde(default)]
    transcript: Option<TranscriptRef>,
    #[serde(default)]
    interrupts: InterruptSignal,
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
    prompts: u64,
    prompt_event: u64,
    heard: SystemTime,
    events: u64,
    asks: Vec<Ask>,
    subagents: BTreeMap<AgentId, Subagent>,
    stopped_early: BTreeMap<AgentId, SystemTime>,
    session: Session,
    transcript: Option<TranscriptRef>,
    interrupts: InterruptSignal,
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
            prompts: activity.prompts,
            prompt_event: activity.prompt_event,
            heard: activity.heard,
            events: activity.events,
            asks: activity.asks,
            subagents: activity.subagents,
            stopped_early: activity.stopped_early,
            session: activity.session,
            transcript: activity.transcript,
            interrupts: activity.interrupts,
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
    heard: SystemTime,
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
            heard: subagent.heard,
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
    heard: Option<SystemTime>,
    #[serde(default)]
    status: Option<SubagentStatus>,
}

impl StoredSubagent {
    /// A file from before `heard` was kept says nothing about when the
    /// subagent was last heard from, so it counts from when its agent last
    /// reported, which is `agent_heard`, rather than showing every one of
    /// them lost at once.
    fn into_subagent(self, agent_heard: SystemTime) -> Subagent {
        let status = self.status.unwrap_or(match self.finished {
            Some(at) => SubagentStatus::Finished { at },
            None => SubagentStatus::Running { tools: self.tools },
        });
        Subagent {
            kind: self.kind,
            description: self.description,
            launch: self.launch,
            since: self.since,
            heard: self.heard.unwrap_or(agent_heard),
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
            prompts,
            prompt_event,
            heard,
            events,
            mut asks,
            subagents,
            stopped_early,
            session,
            transcript,
            interrupts,
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
            prompts,
            prompt_event,
            heard,
            events,
            asks,
            subagents: subagents
                .into_iter()
                .map(|(id, subagent)| (id, subagent.into_subagent(heard)))
                .collect(),
            stopped_early,
            session: session.unwrap_or_else(|| legacy_session(booting, launcher, launched)),
            transcript,
            interrupts,
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
        State::Interrupted => State::Interrupted,
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

    fn stopped() -> SubagentEvent {
        SubagentEvent::Stopped
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
    fn only_a_prompt_from_the_user_begins_an_activity_even_when_it_repeats_the_words() {
        let working = run(vec![
            (0, main(MainEvent::Started)),
            (1, main(user("fix it"))),
        ]);
        let next = |before: &Activity, event: MainEvent| {
            Activity::next(Some(before.clone()), main(event), at(9))
        };
        let done = next(&working, finished(&[]));
        assert!(
            !done.has_new_prompt_since(&working),
            "finishing is not beginning"
        );
        assert!(next(&working, user("and this")).has_new_prompt_since(&working));
        assert!(
            next(&done, user("fix it")).has_new_prompt_since(&done),
            "the same words again"
        );

        let delegating = next(&working, finished(&["a"]));
        let back = next(&delegating, MainEvent::Prompted(Input::Notification));
        assert!(
            !back.has_new_prompt_since(&delegating),
            "a subagent's result"
        );
        let woken = next(&done, MainEvent::Prompted(Input::Notification));
        assert!(!woken.has_new_prompt_since(&done), "a wake-up after rest");
        let restarted = next(&done, MainEvent::Started);
        assert!(
            !restarted.has_new_prompt_since(&done),
            "the session starting again"
        );
    }

    #[test]
    fn an_activity_works_through_tools_and_finishes_unseen() {
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
            (5, sub("b", stopped())),
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
            "a background subagent's prompt outlives the activity"
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
    fn an_activity_left_waiting_on_background_subagents_is_delegating_until_they_report_back() {
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
            (91, sub("a", stopped())),
        ]);
        assert_eq!(
            handed_back.prompt.as_deref(),
            Some("review it"),
            "a notification isn't the user's prompt"
        );
        assert_eq!(handed_back.running(at(92)), 0);
        assert!(
            handed_back.subagents[&AgentId::new("a")].is_finished(),
            "the finished one stays until the next prompt"
        );
        let done = Activity::next(Some(handed_back), main(finished(&[])), at(95));
        assert_eq!(done.state, State::Done { summary: None });
    }

    #[test]
    fn a_subagent_asking_after_the_activity_stays_delegating() {
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

    fn lost_after() -> u64 {
        STALE_AFTER.as_secs() + 1
    }

    #[test]
    fn a_background_subagent_nobody_has_heard_from_stops_counting_as_running() {
        let a = run(vec![
            (0, main(user("review it"))),
            (1, sub("a", launched("t"))),
            (2, main(finished(&["a"]))),
        ]);
        let a_id = AgentId::new("a");
        assert_eq!(a.running(at(2 + STALE_AFTER.as_secs())), 1);
        let later = at(2 + lost_after());
        assert_eq!(a.running(later), 0);
        assert_eq!(
            a.liveness(&a_id, later),
            Some(Liveness::Lost { since: at(2) })
        );
    }

    #[test]
    fn a_quiet_subagent_with_a_tool_running_is_not_lost() {
        let a = run(vec![
            (1, sub("a", launched("t"))),
            (2, sub("a", start("x", "Bash"))),
        ]);
        assert_eq!(a.running(at(2 + 10 * lost_after())), 1);
    }

    #[test]
    fn a_lost_subagent_that_reports_again_is_running_again() {
        let lost = run(vec![(1, sub("a", launched("t")))]);
        assert_eq!(lost.running(at(1 + lost_after())), 0);
        let back = Activity::next(Some(lost), sub("a", started()), at(1 + lost_after()));
        assert_eq!(back.running(at(2 + lost_after())), 1);
    }

    #[test]
    fn a_finished_activity_listing_a_subagent_as_running_vouches_for_it() {
        let a = run(vec![
            (1, sub("a", launched("t"))),
            (2 + lost_after(), main(finished(&["a"]))),
        ]);
        assert_eq!(a.running(at(3 + lost_after())), 1);
    }

    #[test]
    fn a_finished_activity_drops_a_background_subagent_it_does_not_list() {
        let a = run(vec![
            (1, sub("a", launched("t"))),
            (2, sub("b", launched("u"))),
            (3, main(finished(&["b"]))),
        ]);
        assert_eq!(
            a.subagents.keys().map(AgentId::as_str).collect::<Vec<_>>(),
            ["b"]
        );
    }

    #[test]
    fn a_stop_that_arrives_before_the_start_leaves_the_subagent_finished() {
        let a = run(vec![
            (1, sub("a", stopped())),
            (2, sub("a", started())),
            (3, sub("a", start("x", "Bash"))),
        ]);
        assert_eq!(a.running(at(3)), 0);
        assert_eq!(
            a.liveness(&AgentId::new("a"), at(3)),
            Some(Liveness::Finished { at: at(1) })
        );
    }

    #[test]
    fn a_background_launch_after_its_stop_keeps_it_finished_and_described() {
        let a = run(vec![(1, sub("a", stopped())), (2, sub("a", launched("t")))]);
        let subagent = &a.subagents[&AgentId::new("a")];
        assert!(subagent.is_finished());
        assert_eq!(
            (subagent.launch, subagent.label()),
            (Launch::Background, Some("review"))
        );
    }

    #[test]
    fn a_stop_for_a_subagent_nothing_listed_is_never_drawn_or_counted() {
        let a = run(vec![(1, main(user("go"))), (2, sub("late", stopped()))]);
        assert!(a.subagents.is_empty());
        assert_eq!(a.running(at(3)), 0);
    }

    #[test]
    fn a_listing_of_running_background_subagents_cannot_revive_one_that_already_stopped() {
        let a = run(vec![
            (1, main(user("go"))),
            (2, sub("late", stopped())),
            (3, main(finished(&["late"]))),
        ]);
        assert!(a.subagents.is_empty(), "{:?}", a.subagents);
    }

    #[test]
    fn a_new_session_forgets_the_stops_it_was_holding() {
        let a = run(vec![
            (1, sub("a", stopped())),
            (2, main(MainEvent::Started)),
            (3, sub("a", started())),
        ]);
        assert_eq!(a.running(at(3)), 1);
    }

    #[test]
    fn a_subagent_asking_is_waiting_not_lost() {
        let a = run(vec![
            (1, sub("a", launched("t"))),
            (2, sub("a", asked("Bash: rm -rf target"))),
        ]);
        let later = at(2 + 10 * lost_after());
        assert_eq!(a.running(later), 1);
        assert_eq!(a.lost(later), 0);
        let answered = Activity::next(Some(a), sub("a", end("x")), later);
        assert_eq!(answered.lost(later), 0, "answering is hearing from it");
    }

    #[test]
    fn a_subagent_stored_without_when_it_was_heard_counts_from_when_its_agent_last_reported() {
        let time = r#"{"secs_since_epoch":10,"nanos_since_epoch":0}"#;
        let a = old_file(&format!(
            r#","subagents":{{"a":{{"kind":"Explore","description":null,"launch":"background","since":{time},"tools":[]}}}}"#
        ));
        assert_eq!(
            a.lost(at(10 + lost_after())),
            0,
            "counted from when its agent last reported, not from when it began"
        );
        assert_eq!(a.lost(at(20 + lost_after())), 1);
    }

    #[test]
    fn a_subagent_keeps_when_it_was_heard_through_a_round_trip() {
        let a = run(vec![(1, sub("a", launched("t"))), (9, sub("a", started()))]);
        let json = serde_json::to_string(&a).expect("serializes");
        let back: Activity = serde_json::from_str(&json).expect("parses");
        assert_eq!(back.subagents[&AgentId::new("a")].heard, at(9));
    }

    #[test]
    fn a_subagent_that_stops_takes_its_question_with_it() {
        let gone = run(vec![
            (0, sub("a", started())),
            (1, sub("a", asked("Bash: ls"))),
            (2, sub("a", stopped())),
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
            (5, sub("a", stopped())),
            (6, main(end("t"))),
        ]);
        assert_eq!(back.running(at(6)), 0);
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
    fn the_foreground_subagents_of_an_interrupted_activity_are_dropped_by_the_next_prompt() {
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
            "the activity's end says what's still running"
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
            State::Interrupted,
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
    fn a_session_that_only_just_started_gets_time_to_show_a_dialog() {
        let a = run(vec![
            (10, main(finished(&[]))),
            (20, main(MainEvent::Started)),
        ]);
        assert!(matches!(a.state, State::Idle) && a.session.is_booting());
        assert_eq!(a.hold(at(21)), Some(Hold::Booting));
        assert_eq!(a.hold(at(20 + BOOT_QUIET.as_secs())), None);
        let a = run(vec![
            (20, main(MainEvent::Started)),
            (21, main(user("go"))),
            (22, main(finished(&[]))),
        ]);
        assert!(!a.session.is_booting());
        assert_eq!(a.hold(at(23)), None);
    }

    #[test]
    fn a_launched_agent_is_held_until_it_reports_or_the_launch_is_given_up_on() {
        let a = run(vec![(10, main(finished(&[]))), (20, main(fresh(7)))]);
        assert!(matches!(a.state, State::Idle) && a.session.is_booting());
        assert_eq!(a.hold(at(21)), Some(Hold::Booting));
        assert_eq!(
            a.hold(at(20 + LAUNCH_GRACE.as_secs() - 1)),
            Some(Hold::Booting)
        );
        let a = run(vec![
            (10, main(finished(&[]))),
            (20, main(fresh(7))),
            (21, main(MainEvent::Started)),
        ]);
        assert!(matches!(a.state, State::Idle) && a.session.is_booting());
        assert_eq!(a.hold(at(22)), Some(Hold::Booting));
        assert_eq!(a.hold(at(21 + BOOT_QUIET.as_secs())), None);
        let a = run(vec![(10, main(finished(&[]))), (20, main(fresh(7)))]);
        assert_eq!(
            a.hold(at(20 + LAUNCH_GRACE.as_secs())),
            None,
            "a launch that never came up doesn't refuse text for good"
        );
        assert!(a.asks.is_empty() && a.subagents.is_empty());
    }

    #[test]
    fn a_launch_is_not_work_and_is_launching_only_until_the_grace_runs_out() {
        let a = run(vec![(10, main(user("go"))), (20, main(fresh(7)))]);
        assert!(!a.state.is_going(), "{:?}", a.state);
        assert_eq!(a.boot(at(21)), Some(Boot::Launching { since: at(20) }));
        assert_eq!(a.boot(at(20 + LAUNCH_GRACE.as_secs())), None);
        let reported = Activity::next(Some(a), main(MainEvent::Started), at(22));
        assert_eq!(
            reported.boot(at(23)),
            Some(Boot::Starting { since: at(22) })
        );
        assert_eq!(reported.boot(at(22 + BOOT_QUIET.as_secs())), None);
    }

    #[test]
    fn a_working_agent_takes_text_because_the_harness_queues_it() {
        let working = run(vec![(10, main(user("go"))), (11, main(MainEvent::Started))]);
        let working = Activity::next(Some(working), main(user("go")), at(30));
        assert!(matches!(working.state, State::Working { .. }));
        assert_eq!(working.hold(at(31)), None);
        let delegating = run(vec![(10, main(user("go"))), (11, main(finished(&["a"])))]);
        assert!(matches!(delegating.state, State::Delegating));
        assert_eq!(delegating.hold(at(100)), None);
    }

    #[test]
    fn an_open_question_holds_the_text_whoever_asked() {
        let asking = run(vec![(10, main(user("go"))), (11, main(asked("Bash: ls")))]);
        assert_eq!(asking.hold(at(100)), Some(Hold::Asking));
        let sub_asking = run(vec![
            (10, main(user("go"))),
            (11, sub("a", started())),
            (12, sub("a", asked("Bash: ls"))),
        ]);
        assert_eq!(sub_asking.hold(at(100)), Some(Hold::Asking));
        let answered = Activity::next(Some(asking), main(end("t")), at(101));
        assert_eq!(answered.hold(at(102)), None);
    }

    #[test]
    fn an_ended_session_holds_the_text_so_it_cannot_reach_a_shell() {
        let ended = run(vec![(10, main(user("go"))), (11, main(MainEvent::Ended))]);
        assert_eq!(ended.hold(at(100)), Some(Hold::Ended));
    }

    fn working_on_a_prompt() -> Activity {
        run(vec![(10, main(user("go"))), (11, main(started_tool("t1")))])
    }

    fn started_tool(id: &str) -> Common {
        Common::ToolStarted {
            id: ToolId::new(id),
            tool: "Bash".into(),
            target: None,
        }
    }

    fn interrupted(working: Activity) -> Activity {
        let proof = working.going().expect("going");
        working.interrupted(proof, at(20))
    }

    fn prompted_again(activity: Activity) -> Activity {
        Activity::next(Some(activity), main(user("and another")), at(15))
    }

    #[test]
    fn an_interrupt_proved_at_the_current_report_takes_a_working_agent_to_rest() {
        let stopped = interrupted(working_on_a_prompt());
        assert_eq!(stopped.state, State::Interrupted);
        assert_eq!(stopped.since, at(20));
        assert_eq!(stopped.hold(at(21)), None);
    }

    #[test]
    fn a_delegating_agent_is_going_and_an_interrupt_ends_it() {
        let delegating = run(vec![
            (10, main(user("go"))),
            (11, sub("a", launched("t"))),
            (12, main(finished(&["a"]))),
        ]);
        assert!(matches!(delegating.state, State::Delegating));
        assert_eq!(interrupted(delegating).state, State::Interrupted);
    }

    #[test]
    fn an_interrupt_proved_before_a_newer_prompt_cannot_idle_the_newer_turn() {
        let working = working_on_a_prompt();
        let stale = working.going().expect("going");
        let newer = prompted_again(working);
        let after = newer.clone().interrupted(stale, at(20));
        assert_eq!(after, newer, "unchanged");
        assert!(matches!(after.state, State::Working { .. }));
    }

    #[test]
    fn an_interrupt_proved_before_any_later_report_is_dropped_too() {
        let working = working_on_a_prompt();
        let stale = working.going().expect("going");
        let later = Activity::next(Some(working), main(started_tool("t2")), at(15));
        assert_eq!(later.clone().interrupted(stale, at(20)), later);
    }

    #[test]
    fn only_a_going_activity_can_be_proved_going_and_only_an_interrupted_one_stopped() {
        for state in [
            State::Idle,
            State::Done { summary: None },
            State::Failed {
                reason: "overloaded".into(),
            },
            State::Exited,
        ] {
            let mut resting = working_on_a_prompt();
            resting.state = state;
            assert_eq!(resting.standing(), None);
        }
        let working = working_on_a_prompt();
        assert!(matches!(working.standing(), Some(Standing::Going(_))));
        assert!(working.stopped().is_none());
        let stopped = interrupted(working);
        assert!(matches!(stopped.standing(), Some(Standing::Stopped(_))));
        assert!(stopped.going().is_none());
    }

    #[test]
    fn the_proof_says_from_when_a_transcript_record_counts() {
        let working = working_on_a_prompt();
        assert_eq!(working.standing().map(Standing::after), Some(at(10)));
        let stopped = interrupted(working);
        assert_eq!(stopped.standing().map(Standing::after), Some(at(20)));
    }

    #[test]
    fn an_interrupt_closes_questions_and_foreground_subagents_but_not_background_ones() {
        let working = run(vec![
            (10, main(user("go"))),
            (11, sub("fg", started())),
            (
                12,
                Reported::subagent(
                    AgentId::new("bg"),
                    SubagentEvent::Launched {
                        tool: ToolId::new("launch"),
                        kind: "Explore".into(),
                        description: "look".into(),
                    },
                ),
            ),
            (13, sub("fg", asked("Bash: ls"))),
            (14, sub("bg", asked("Read"))),
            (15, main(asked("Edit"))),
        ]);
        assert_eq!(working.asks.len(), 3);
        let stopped = interrupted(working);
        assert_eq!(
            stopped
                .subagents
                .keys()
                .map(AgentId::as_str)
                .collect::<Vec<_>>(),
            ["bg"]
        );
        assert_eq!(questions(&stopped), ["Read"]);
        assert_eq!(
            stopped.running(at(20)),
            1,
            "a background subagent still runs"
        );
    }

    #[test]
    fn a_tool_finishing_late_does_not_wake_an_interrupted_agent() {
        let stopped = interrupted(working_on_a_prompt());
        let late = Activity::next(Some(stopped), main(end("t1")), at(21));
        assert_eq!(late.state, State::Interrupted);
    }

    #[test]
    fn the_next_prompt_after_an_interrupt_starts_a_new_activity() {
        let next = Activity::next(
            Some(interrupted(working_on_a_prompt())),
            main(user("try again")),
            at(30),
        );
        assert!(matches!(next.state, State::Working { .. }));
        assert_eq!(next.prompt.as_deref(), Some("try again"));
    }

    #[test]
    fn an_interrupted_agent_goes_back_to_work_only_under_a_proof_that_is_still_current() {
        let stopped = interrupted(working_on_a_prompt());
        let proof = stopped.stopped().expect("stopped");
        let resumed = stopped.clone().resumed(proof, at(40));
        assert!(matches!(resumed.state, State::Working { ref tools } if tools.is_empty()));
        assert_eq!(resumed.since, at(40));
        let moved_on = Activity::next(Some(stopped), main(user("again")), at(35));
        assert_eq!(moved_on.clone().resumed(proof, at(40)), moved_on);
    }

    #[test]
    fn a_tool_that_just_started_does_not_hold_typing() {
        let working = working_on_a_prompt();
        assert_eq!(working.hold(at(11)), None);
        let delegating = run(vec![
            (10, main(user("go"))),
            (11, sub("a", started())),
            (12, sub("a", started_tool("s1"))),
        ]);
        assert_eq!(delegating.hold(at(12)), None);
    }

    #[test]
    fn reports_from_a_replaced_process_are_ignored_after_a_launch() {
        let from = |pids: &[u32], event| {
            main(event).from_lineage(Lineage::Traced(
                pids.iter().filter_map(|p| Pid::new(*p)).collect(),
            ))
        };
        let a = run(vec![
            (10, main(finished(&[]))),
            (20, main(fresh(7))),
            (21, from(&[99, 98], finished(&[]))),
        ]);
        assert!(matches!(a.state, State::Idle) && a.session.is_booting());
        assert_eq!(a.heard, at(20));
        let a = run(vec![
            (20, main(fresh(7))),
            (21, from(&[50, 7, 3], MainEvent::Started)),
        ]);
        assert!(matches!(a.state, State::Idle));
    }

    #[test]
    fn untraceable_or_unrecorded_reports_are_accepted() {
        let a = run(vec![(20, main(fresh(7))), (21, main(MainEvent::Started))]);
        assert!(matches!(a.state, State::Idle), "no lineage");
        let a = run(vec![
            (20, main(untraced())),
            (
                21,
                main(MainEvent::Started).from_lineage(Lineage::Traced(vec![pid(99)])),
            ),
        ]);
        assert!(matches!(a.state, State::Idle), "reattached session");
        let a = run(vec![(
            21,
            main(MainEvent::Started).from_lineage(Lineage::Traced(vec![pid(99)])),
        )]);
        assert!(matches!(a.state, State::Idle), "old record");
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
            (1, sub("a", stopped())),
            (2, sub("a", start("x", "Bash"))),
        ]);
        let subagent = &a.subagents[&AgentId::new("a")];
        assert_eq!(
            (subagent.status.clone(), subagent.tools().len()),
            (SubagentStatus::Finished { at: at(1) }, 0)
        );
        assert!(!a.stale(at(3)) && a.running(at(3)) == 0);
        let working = run(vec![(0, main(user("go")))]);
        assert!(working.stale(at(400)), "no subagent keeps it busy");
        let a = run(vec![
            (0, main(user("go"))),
            (1, sub("a", launched("t"))),
            (2, sub("a", stopped())),
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
        assert!(matches!(
            running.liveness(at(0), Waiting::Off),
            Liveness::Running { tools } if tools.len() == 1
        ));
        let finished = subagent(&format!(r#","tools":[],"finished":{time}"#));
        assert_eq!(
            finished.liveness(at(0), Waiting::Off),
            Liveness::Finished { at: at(20) }
        );
        let bare = subagent("");
        assert!(matches!(
            bare.liveness(at(0), Waiting::Off),
            Liveness::Running { tools } if tools.is_empty()
        ));
    }

    #[test]
    fn the_session_survives_a_round_trip() {
        let a = run(vec![
            (0, main(fresh(7))),
            (1, sub("a", launched("t"))),
            (2, sub("a", stopped())),
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
        assert!(!a.session.is_booting());
    }

    #[test]
    fn the_latest_transcript_reported_is_kept_through_reports_without_one_and_a_save() {
        let first = TranscriptRef::new("/p/one.jsonl");
        let second = TranscriptRef::new("/p/two.jsonl");
        let a = run(vec![
            (1, main(user("go")).at_transcript(Some(first.clone()))),
            (2, main(MainEvent::Common(start("t", "Bash")))),
        ]);
        assert_eq!(a.transcript, Some(first));
        let b = Activity::next(
            Some(a),
            main(user("again")).at_transcript(Some(second.clone())),
            at(3),
        );
        assert_eq!(
            b.transcript,
            Some(second.clone()),
            "a new session names its own"
        );
        let saved = serde_json::to_string(&b).expect("serialises");
        let loaded: Activity = serde_json::from_str(&saved).expect("reads");
        assert_eq!(loaded.transcript, Some(second));
        let old = saved.replace(r#""transcript":"/p/two.jsonl","#, "");
        assert_eq!(
            serde_json::from_str::<Activity>(&old)
                .expect("a file from before it")
                .transcript,
            None
        );
    }
}
