//! What each agent is doing, as its harness reports it through hooks
//! (`agent event`), so the TUI can show it and say which agents need you.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// No event for this long while working, with no tool running, looks stuck.
/// Thinking through a long answer can go a few minutes without an event,
/// and a running tool (a test suite, a build) can take as long as it likes.
pub const STALE_AFTER: Duration = Duration::from_secs(300);

/// Something a harness reported about an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A session began (fresh, resumed or cleared).
    Started,
    /// The user sent a prompt.
    Prompted(String),
    /// A tool began: its id, its name, and what it's acting on when that's
    /// short.
    ToolStarted {
        id: String,
        tool: String,
        target: Option<String>,
    },
    /// A tool finished, failed, or was denied permission.
    ToolFinished { id: String },
    /// The agent is waiting on the user: a permission prompt or a question.
    Asked(String),
    /// The agent is busy without that changing what it's doing: a
    /// subagent's tool calls, or compacting its context.
    Busy,
    /// The turn ended. `summary` is the agent's last message.
    Finished(Option<String>),
    /// The turn ended in an error (rate limited, overloaded…).
    Failed(String),
    /// The session ended.
    Ended,
}

/// What an agent is doing.
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
    /// Waiting on the user.
    Asking {
        question: String,
    },
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
    pub id: String,
    pub name: String,
    pub target: Option<String>,
    pub since: SystemTime,
}

/// An agent's state, since when, and what it was last asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub state: State,
    pub since: SystemTime,
    pub prompt: Option<String>,
    /// When the harness last reported anything.
    pub heard: SystemTime,
    /// Counts the harness's reports, so a view can show progress as it
    /// happens.
    pub events: u64,
}

impl Activity {
    /// The activity after `event` happened at `now`. The time in state only
    /// restarts when the state itself changes, not when a working agent
    /// moves to its next tool.
    pub fn next(previous: Option<Self>, event: Event, now: SystemTime) -> Self {
        let (prompt, events, before) = match previous {
            Some(Self {
                prompt,
                events,
                state,
                since,
                ..
            }) => (prompt, events + 1, Some((state, since))),
            None => (None, 1, None),
        };
        let running = || match before.as_ref().map(|(state, _)| state) {
            Some(State::Working { tools }) => tools.clone(),
            _ => Vec::new(),
        };
        let (state, prompt) = match event {
            Event::Started => (State::Idle, prompt),
            Event::Prompted(text) => (State::Working { tools: Vec::new() }, Some(text)),
            Event::ToolStarted { id, tool, target } => {
                let mut tools = running();
                tools.retain(|t| t.id != id);
                tools.push(Tool {
                    id,
                    name: tool,
                    target,
                    since: now,
                });
                (State::Working { tools }, prompt)
            }
            Event::ToolFinished { id } => {
                let mut tools = running();
                tools.retain(|t| t.id != id);
                (State::Working { tools }, prompt)
            }
            Event::Asked(question) => (State::Asking { question }, prompt),
            // Activity after a permission prompt means it was answered.
            Event::Busy => match before.as_ref().map(|(state, _)| state.clone()) {
                Some(State::Asking { .. }) | None => (State::Working { tools: Vec::new() }, prompt),
                Some(state) => (state, prompt),
            },
            Event::Finished(summary) => (State::Done { summary }, prompt),
            Event::Failed(reason) => (State::Failed { reason }, prompt),
            Event::Ended => (State::Exited, prompt),
        };
        let since = match before {
            Some((previous, since)) if same_kind(&previous, &state) => since,
            _ => now,
        };
        Self {
            state,
            since,
            prompt,
            heard: now,
            events,
        }
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

    /// Working with no tool running and nothing heard for [`STALE_AFTER`]:
    /// probably interrupted (Esc fires no hook) or stuck.
    pub fn stale(&self, now: SystemTime) -> bool {
        matches!(&self.state, State::Working { tools } if tools.is_empty())
            && now.duration_since(self.heard).unwrap_or_default() > STALE_AFTER
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

    fn run(events: Vec<(u64, Event)>) -> Activity {
        let mut activity = None;
        for (secs, event) in events {
            activity = Some(Activity::next(activity, event, at(secs)));
        }
        activity.unwrap()
    }

    fn start(id: &str, tool: &str) -> Event {
        Event::ToolStarted {
            id: id.into(),
            tool: tool.into(),
            target: None,
        }
    }

    #[test]
    fn a_turn_works_through_tools_and_finishes_unseen() {
        let activity = run(vec![
            (0, Event::Started),
            (10, Event::Prompted("fix it".into())),
            (12, start("t1", "Bash")),
            (40, Event::ToolFinished { id: "t1".into() }),
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
            Event::Finished(Some("Fixed.".into())),
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
            (0, start("a", "Task")),
            (1, start("b", "Task")),
            (5, Event::ToolFinished { id: "a".into() }),
        ]);
        assert_eq!(activity.current_tool().map(|t| t.id.as_str()), Some("b"));
        assert!(!activity.stale(at(900)), "the other task is still running");
    }

    #[test]
    fn an_answered_prompt_moves_on_when_anything_happens() {
        let approved = run(vec![
            (0, Event::Asked("Bash: cargo build".into())),
            (3, Event::Busy),
        ]);
        assert_eq!(approved.state, State::Working { tools: Vec::new() });
        let ran = run(vec![
            (0, Event::Asked("Bash: cargo build".into())),
            (3, Event::ToolFinished { id: "x".into() }),
        ]);
        assert_eq!(ran.state, State::Working { tools: Vec::new() });
    }

    #[test]
    fn busy_keeps_what_it_was_doing() {
        let compacting = run(vec![(0, start("t", "Read")), (5, Event::Busy)]);
        assert_eq!(
            compacting.current_tool().map(|t| t.name.as_str()),
            Some("Read")
        );
        assert_eq!((compacting.heard, compacting.events), (at(5), 2));
    }

    #[test]
    fn seeing_only_clears_done() {
        let asking = run(vec![(0, Event::Asked("run rm?".into()))]).acknowledged(at(5));
        assert_eq!(
            (asking.state, asking.since),
            (
                State::Asking {
                    question: "run rm?".into()
                },
                at(0)
            )
        );
    }

    #[test]
    fn only_an_idle_wait_without_a_tool_goes_stale() {
        let thinking = run(vec![(0, Event::Prompted("go".into()))]);
        assert!(!thinking.stale(at(120)));
        assert!(thinking.stale(at(400)));
        let building = run(vec![(0, start("t", "Bash"))]);
        assert!(!building.stale(at(3600)), "a long build isn't stuck");
    }
}
