//! What each agent is doing, as its harness reports it through hooks
//! (`agent event`), so the TUI can show it and say which agents need you.

use std::collections::BTreeMap;
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
    pub origin: Origin,
    pub event: Event,
}

impl Reported {
    pub fn main(event: Event) -> Self {
        Self {
            origin: Origin::Main,
            event,
        }
    }

    pub fn subagent(id: AgentId, event: Event) -> Self {
        Self {
            origin: Origin::Subagent(id),
            event,
        }
    }
}

/// Something a harness reported about an agent or one of its subagents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A session began (fresh, resumed or cleared).
    Started,
    /// The context was compacted.
    Compacted,
    Prompted(Input),
    /// A tool began: its id, its name, and what it's acting on when that's
    /// short.
    ToolStarted {
        id: ToolId,
        tool: String,
        target: Option<String>,
    },
    /// A tool finished, failed, or was denied permission.
    ToolFinished {
        id: ToolId,
    },
    /// Waiting on the user: a permission prompt or a question.
    Asked {
        question: String,
        about: About,
    },
    /// The user turned down the asker's permission prompt.
    Declined,
    /// The subagent the event came from started.
    SubagentStarted {
        kind: String,
    },
    /// Tool call `tool` handed the subagent the event is about off to run in
    /// the background.
    SubagentLaunched {
        tool: ToolId,
        kind: String,
        description: String,
    },
    /// The subagent the event came from finished.
    SubagentStopped,
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
pub struct Subagent {
    pub kind: String,
    pub description: Option<String>,
    pub launch: Launch,
    /// The tools it's running.
    pub tools: Vec<Tool>,
    pub since: SystemTime,
}

/// Whether the agent waits on a subagent's tool call or carries on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Launch {
    Foreground,
    Background,
}

/// An agent's state, since when, what it was last asked, and what it's
/// waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Stored")]
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
}

impl Activity {
    /// The activity after `reported` happened at `now`. The time in state
    /// only restarts when the state itself changes, not when a working agent
    /// moves to its next tool or answers a question.
    pub fn next(previous: Option<Self>, reported: Reported, now: SystemTime) -> Self {
        let mut activity = previous.unwrap_or(Self {
            state: State::Working { tools: Vec::new() },
            since: now,
            prompt: None,
            heard: now,
            events: 0,
            asks: Vec::new(),
            subagents: BTreeMap::new(),
        });
        let before = activity.state.clone();
        let answered = activity.answer(&reported);
        activity.apply(reported, answered, now);
        if !same_kind(&before, &activity.state) {
            activity.since = now;
        }
        activity.heard = now;
        activity.events += 1;
        activity
    }

    /// Takes out the question `reported` shows was answered.
    fn answer(&mut self, reported: &Reported) -> Option<Ask> {
        let at = self.asks.iter().position(|ask| {
            ask.asker == reported.origin
                && match (&reported.event, &ask.answered_by) {
                    (Event::ToolFinished { id }, Answer::ToolEnding(tool)) => id == tool,
                    (Event::ToolStarted { .. } | Event::ToolFinished { .. }, Answer::AnyTool)
                    | (
                        Event::Declined
                        | Event::Prompted(_)
                        | Event::Failed(_)
                        | Event::SubagentStopped,
                        _,
                    ) => true,
                    _ => false,
                }
        })?;
        Some(self.asks.remove(at))
    }

    fn apply(
        &mut self,
        Reported { origin, event }: Reported,
        answered: Option<Ask>,
        now: SystemTime,
    ) {
        let declined = match answered {
            Some(Ask {
                answered_by: Answer::ToolEnding(id),
                ..
            }) => Some(id),
            _ => None,
        };
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match (origin, event) {
            (Origin::Main, Event::Started) => {
                self.forget_subagents();
                State::Idle
            }
            (Origin::Main, Event::Ended) => {
                self.forget_subagents();
                State::Exited
            }
            (Origin::Main, Event::Prompted(input)) => {
                if let Input::User(text) = input {
                    self.prompt = Some(text);
                    self.subagents.retain(|_, s| s.launch == Launch::Background);
                    let subagents = &self.subagents;
                    self.asks.retain(|ask| match &ask.asker {
                        Origin::Subagent(id) => subagents.contains_key(id),
                        Origin::Main => false,
                    });
                }
                State::Working { tools: Vec::new() }
            }
            (Origin::Main, Event::ToolStarted { id, tool, target }) => {
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
            (Origin::Main, Event::Declined) => match declined {
                Some(id) => finish(state, &id),
                None => state,
            },
            (Origin::Main, Event::ToolFinished { id }) => finish(state, &id),
            (Origin::Main, Event::Finished { summary, running }) => {
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
            (Origin::Main, Event::Failed(reason)) => State::Failed { reason },
            (asker, Event::Asked { question, about }) => {
                let answered_by = self.answered_by(&asker, about, &state);
                self.asks.retain(|ask| ask.asker != asker);
                self.asks.push(Ask {
                    asker,
                    question,
                    answered_by,
                    since: now,
                });
                state
            }
            (Origin::Subagent(id), Event::SubagentStarted { kind }) => {
                self.subagents.entry(id).or_insert(Subagent {
                    kind,
                    description: None,
                    launch: Launch::Foreground,
                    tools: Vec::new(),
                    since: now,
                });
                state
            }
            (
                Origin::Subagent(id),
                Event::SubagentLaunched {
                    tool,
                    kind,
                    description,
                },
            ) => {
                let subagent = self.subagents.entry(id).or_insert(Subagent {
                    kind,
                    description: None,
                    launch: Launch::Background,
                    tools: Vec::new(),
                    since: now,
                });
                subagent.launch = Launch::Background;
                subagent.description = Some(description);
                finish(state, &tool)
            }
            (Origin::Subagent(id), Event::SubagentStopped) => {
                self.subagents.remove(&id);
                state
            }
            (
                Origin::Subagent(id),
                Event::ToolStarted {
                    id: tool,
                    tool: name,
                    target,
                },
            ) => {
                if let Some(subagent) = self.subagents.get_mut(&id) {
                    subagent.tools.push(Tool {
                        id: tool,
                        name,
                        target,
                        since: now,
                    });
                }
                state
            }
            (Origin::Subagent(id), Event::ToolFinished { id: tool }) => {
                if let Some(subagent) = self.subagents.get_mut(&id) {
                    subagent.tools.retain(|t| t.id != tool);
                }
                state
            }
            (Origin::Subagent(id), Event::Declined) => {
                if let (Some(subagent), Some(tool)) = (self.subagents.get_mut(&id), declined) {
                    subagent.tools.retain(|t| t.id != tool);
                }
                state
            }
            (
                Origin::Main,
                Event::Compacted
                | Event::SubagentStarted { .. }
                | Event::SubagentLaunched { .. }
                | Event::SubagentStopped,
            )
            | (Origin::Subagent(_), _) => state,
        };
    }

    /// The finished turn's background subagents replace the ones tracked.
    fn track_background(&mut self, running: &[Background], now: SystemTime) {
        self.subagents
            .retain(|id, _| running.iter().any(|b| &b.id == id));
        for b in running {
            let subagent = self
                .subagents
                .entry(b.id.clone())
                .or_insert_with(|| Subagent {
                    kind: b.kind.clone(),
                    description: None,
                    launch: Launch::Background,
                    tools: Vec::new(),
                    since: now,
                });
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
            (Origin::Subagent(id), _) => self.subagents.get(id).map_or(&[], |s| &s.tools),
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
    /// and nothing heard for [`STALE_AFTER`]: probably interrupted (Esc fires
    /// no hook), stuck, or its subagents died with the session.
    pub fn stale(&self, now: SystemTime) -> bool {
        let quiet = now.duration_since(self.heard).unwrap_or_default() > STALE_AFTER;
        let subagent_busy = self.subagents.values().any(|s| !s.tools.is_empty());
        match &self.state {
            State::Working { tools } => tools.is_empty() && !subagent_busy && quiet,
            State::Delegating => !subagent_busy && quiet,
            _ => false,
        }
    }
}

/// An activity file as written by any version, including ones that kept a
/// single question in the state.
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
        activity.unwrap()
    }

    fn main(event: Event) -> Reported {
        Reported::main(event)
    }

    fn sub(id: &str, event: Event) -> Reported {
        Reported::subagent(AgentId::new(id), event)
    }

    fn start(id: &str, tool: &str) -> Event {
        Event::ToolStarted {
            id: ToolId::new(id),
            tool: tool.into(),
            target: None,
        }
    }

    fn end(id: &str) -> Event {
        Event::ToolFinished {
            id: ToolId::new(id),
        }
    }

    fn user(text: &str) -> Event {
        Event::Prompted(Input::User(text.into()))
    }

    fn asked(question: &str) -> Event {
        Event::Asked {
            question: question.into(),
            about: About::Unknown,
        }
    }

    fn finished(running: &[&str]) -> Event {
        Event::Finished {
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

    fn started() -> Event {
        Event::SubagentStarted {
            kind: "general-purpose".into(),
        }
    }

    fn launched(tool: &str) -> Event {
        Event::SubagentLaunched {
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
            (0, main(Event::Started)),
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
            main(Event::Finished {
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
                main(Event::Asked {
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
            (5, sub("b", Event::SubagentStopped)),
            (6, main(Event::Compacted)),
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
                main(Event::Asked {
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
        let declined = Activity::next(Some(asking), main(Event::Declined), at(4));
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
            (90, main(Event::Prompted(Input::Notification))),
            (91, sub("a", Event::SubagentStopped)),
        ]);
        assert_eq!(
            handed_back.prompt.as_deref(),
            Some("review it"),
            "a notification isn't the user's prompt"
        );
        assert_eq!(handed_back.subagents, BTreeMap::new());
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
            (2, sub("a", Event::SubagentStopped)),
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
            (5, sub("a", Event::SubagentStopped)),
            (6, main(end("t"))),
        ]);
        assert_eq!(back.subagents, BTreeMap::new());
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
            (9, main(Event::Started)),
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
            (5, main(Event::Compacted)),
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
        let activity: Activity = serde_json::from_str(&old).unwrap();
        assert_eq!(
            (activity.events, questions(&activity)),
            (3, vec!["run rm?"])
        );
        let working = format!(
            r#"{{"state":{{"kind":"working","tools":[]}},"since":{time},"prompt":"go","heard":{time},"events":1}}"#
        );
        let activity: Activity = serde_json::from_str(&working).unwrap();
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
            let json = serde_json::to_string(&activity).unwrap();
            assert_eq!(
                serde_json::from_str::<Activity>(&json).unwrap(),
                activity,
                "{json}"
            );
        }
    }
}
