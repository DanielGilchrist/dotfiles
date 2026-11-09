//! What the model remembers per agent and per subagent, each kept in one
//! record so pruning one drops everything known about it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use agent_core::activity::{AgentId, Standing, Subagent};
use agent_core::names::Prompt;
use agent_core::ports::PromptKind;
use agent_core::ports::{ActivityDetail, Log, TranscriptStamp, WorktreeFacts};

use crate::log::Nav;
use crate::model::{Company, Held, Inspection};
use crate::touched::Baseline;
use crate::tree::{ChangesScope, ChangesView, TreeRow};

/// The tree built for an agent, rebuilt only when its facts, the view or
/// its closed folders change.
pub struct CachedTree {
    pub generation: u64,
    pub view: ChangesView,
    pub scope: ChangesScope,
    pub collapsed: HashSet<PathBuf>,
    pub rows: Arc<Vec<TreeRow>>,
}

/// When a running session was first seen live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSince {
    /// Already live at the first snapshot, whose start isn't known.
    Unknown,
    Since(SystemTime),
}

/// Everything the model keeps for one agent, dropped together when the
/// agent leaves the snapshot.
#[derive(Default)]
pub struct Tracked {
    /// Its git facts, or why they couldn't be read, as last read.
    pub facts: Option<Result<Inspected, String>>,
    /// How many times its facts have been read, which tells a document read
    /// from them that they may have changed.
    pub generation: u64,
    /// The folders its [3] has closed. A folder that isn't named is open,
    /// so one that appears starts open.
    pub collapsed: HashSet<PathBuf>,
    pub tree: Option<CachedTree>,
    /// When its facts were last asked for.
    pub inspection: Option<Inspection>,
    /// Comments drafted on diffs, until sent.
    pub draft: Vec<crate::review::Comment>,
    /// Messages waiting to be typed in.
    pub outbox: Option<Outbox>,
    pub live_since: Option<LiveSince>,
    /// Whether a snapshot has listed it, and when this TUI last started or
    /// resurrected its session.
    pub seen: Seen,
    /// The subagents being watched, dropped with the agent.
    pub subagents: HashMap<AgentId, Watched>,
    /// The other windows showing its session, as the newest look found them.
    pub company: Company,
    /// Its current activity, as [0] last read it.
    pub activity_view: ActivityWatch,
    /// Where reading its transcript for an interrupt stands.
    pub probed: Option<Probed>,
    /// The last message typed into it.
    pub sent: Option<Sent>,
    /// Its changed files when that activity began.
    pub baseline: Baseline,
}

/// What reading an agent's worktree found, and when in its reports it was
/// asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspected {
    pub facts: WorktreeFacts,
    /// The checkout with symlinks resolved.
    pub real_root: Option<PathBuf>,
    /// How many events the agent had reported when the read was asked for.
    pub asked_events: Option<u64>,
}

impl Inspected {
    /// Facts with nothing else known about the read.
    #[cfg(test)]
    pub fn of(facts: WorktreeFacts) -> Self {
        Self {
            facts,
            real_root: None,
            asked_events: None,
        }
    }
}

impl Tracked {
    /// Opens the folders in `folders`, which are closed or not.
    pub fn open_folders(&mut self, folders: &[PathBuf]) {
        for folder in folders {
            self.collapsed.remove(folder);
        }
    }
}

/// Where reading an agent's transcript for an interrupt stands: for which
/// proof it was asked, when, and what the last answer left to know. Anything
/// found only holds for the proof it was found under, since a report since
/// then makes the verdict need taking again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    pub standing: Standing,
    pub at: Instant,
    pub found: Found,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// No answer yet, or none that says anything about the file.
    Nothing,
    /// The file as the last answer found it.
    Stamp(TranscriptStamp),
    /// The last read failed, and the user was told.
    Failed,
    /// There is no transcript to read.
    NoTranscript,
}

impl Probed {
    /// Whether a read for `standing` is due at `now`, given the last one.
    pub fn due(last: Option<&Self>, standing: Standing, now: Instant) -> bool {
        match last.filter(|p| p.standing == standing) {
            None => true,
            Some(p) => p.found != Found::NoTranscript && now >= p.at + PROBE_EVERY,
        }
    }

    /// What to keep for a read asked for at `now`, and the stamp to ask it
    /// with: what the last one left to know, if it was for the same proof.
    pub fn asked(
        last: Option<Self>,
        standing: Standing,
        now: Instant,
    ) -> (Self, Option<TranscriptStamp>) {
        let (found, seen) = match last.filter(|p| p.standing == standing).map(|p| p.found) {
            Some(Found::Stamp(stamp)) => (Found::Stamp(stamp.clone()), Some(stamp)),
            Some(Found::Failed) => (Found::Failed, None),
            Some(Found::Nothing | Found::NoTranscript) | None => (Found::Nothing, None),
        };
        (
            Self {
                standing,
                at: now,
                found,
            },
            seen,
        )
    }
}

/// How often a working agent's transcript is read for an interrupt.
pub const PROBE_EVERY: Duration = Duration::from_secs(2);

/// How long after a message is typed the next waits for the agent to report.
pub const JUST_SENT_FOR: Duration = Duration::from_secs(2);

/// How long after a command is typed, which reports nothing, the next
/// message waits for the agent to report.
pub const COMMAND_HOLD_FOR: Duration = Duration::from_secs(15);

/// The last message typed into an agent: how many events it had reported
/// then, when, and what it was.
#[derive(Debug, Clone, Copy)]
pub struct Sent {
    pub events: u64,
    pub at: Instant,
    pub kind: PromptKind,
}

impl Sent {
    /// Why the agent shouldn't be typed into again yet, if it shouldn't.
    /// Hooks lag the typing, so until it has reported something it can't be
    /// told whether the message opened a menu.
    pub fn hold(&self, events: Option<u64>, now: Instant) -> Option<Held> {
        if events != Some(self.events) {
            return None;
        }
        let (lasts, hold) = match self.kind {
            PromptKind::Message => (JUST_SENT_FOR, Held::JustSent),
            PromptKind::Command => (COMMAND_HOLD_FOR, Held::Command),
        };
        (now.saturating_duration_since(self.at) < lasts).then_some(hold)
    }
}

/// What is known about an agent's current activity, and how far the user had
/// got when they last looked.
#[derive(Default)]
pub struct ActivityWatch {
    pub watch: Watch<ActivityDetail>,
    pub looked: Looked,
}

/// When a log's transcript was last asked for, and how many events its
/// agent had reported then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadAt {
    pub events: u64,
    pub at: Instant,
}

/// A log [0] reads: when it was last asked for, what came back, and the
/// cursor over it.
pub struct Watch<T> {
    pub read: Option<ReadAt>,
    pub log: Option<Result<Option<T>, String>>,
    pub nav: Nav,
}

impl<T> Default for Watch<T> {
    fn default() -> Self {
        Self {
            read: None,
            log: None,
            nav: Nav::default(),
        }
    }
}

/// A log is read again on its agent's activity, at most this often.
pub const LOG_REFRESH: Duration = Duration::from_secs(2);
/// And this often while it is growing whatever its agent reports, since a
/// transcript can be written without a hook firing.
pub const LOG_SLOW_REFRESH: Duration = Duration::from_secs(10);

impl<T> Watch<T> {
    /// Whether to ask for the log at `now`, when its agent has reported
    /// `events` and `growing` says its transcript may be getting longer.
    pub fn due(&self, events: u64, growing: bool, now: Instant) -> bool {
        self.read.is_none_or(|last| {
            (last.events != events && now >= last.at + LOG_REFRESH)
                || (growing && now >= last.at + LOG_SLOW_REFRESH)
        })
    }

    /// It was asked for at `now`, with `events` reported.
    pub fn asked(&mut self, events: u64, now: Instant) {
        self.read = Some(ReadAt { events, at: now });
    }
}

/// Whether the activity is in front of the user, and when they last saw it.
#[derive(Debug, Default)]
pub struct Looked {
    /// When [0] began showing the activity, while it does.
    pub since: Option<Instant>,
    /// The last moment the activity view or the terminal had been in front of
    /// them for a few seconds. What the agent did after is
    /// new to them.
    pub seen: Option<SystemTime>,
}

/// How long a launch is kept for a session no snapshot lists: its worktree
/// can take a while to appear, but not forever.
pub const PENDING_LAUNCH_FOR: Duration = Duration::from_mins(5);

/// Whether an agent has been listed by a snapshot, and its launch by this TUI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Seen {
    /// Not launched here and not listed yet.
    #[default]
    Unlaunched,
    /// Launched here and waiting for a snapshot to list it.
    Pending {
        launched_at: SystemTime,
    },
    Listed {
        launched_at: Option<SystemTime>,
    },
}

impl Seen {
    pub fn launched_at(self) -> Option<SystemTime> {
        match self {
            Self::Unlaunched => None,
            Self::Pending { launched_at } => Some(launched_at),
            Self::Listed { launched_at } => launched_at,
        }
    }

    /// Whether a launch is still waiting to be listed at `now`.
    pub fn pending(self, now: SystemTime) -> bool {
        matches!(self, Self::Pending { launched_at }
            if now.duration_since(launched_at).is_ok_and(|age| age <= PENDING_LAUNCH_FOR) || now < launched_at)
    }
}

impl Tracked {
    /// Its session was started or resurrected here at `at`.
    pub fn launched(&mut self, at: SystemTime) {
        self.seen = match self.seen {
            Seen::Listed { .. } => Seen::Listed {
                launched_at: Some(at),
            },
            Seen::Unlaunched | Seen::Pending { .. } => Seen::Pending { launched_at: at },
        };
    }

    /// A snapshot lists it.
    pub fn listed(&mut self) {
        self.seen = Seen::Listed {
            launched_at: self.seen.launched_at(),
        };
    }
}

/// What is known about one subagent being watched.
#[derive(Default)]
pub struct Watched {
    pub watch: Watch<Log>,
    /// The subagent as its agent last listed it, kept so a new prompt
    /// clearing finished subagents doesn't take its row from under the
    /// cursor. Marked finished once the activity drops it, since nothing
    /// will say so afterwards.
    pub held: Option<Subagent>,
}

/// A message waiting to be typed, with the review comments it carries so a
/// message that is never typed in can give them back.
#[derive(Debug)]
pub struct Queued {
    pub prompt: Prompt,
    pub comments: Vec<crate::review::Comment>,
}

/// How long a refusal lasts with no new activity before the worker is asked
/// again, which rechecks the store itself.
pub const REFUSED_FOR: Duration = Duration::from_secs(5);

/// What lifts a refusal before [`REFUSED_FOR`] has passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// The next read of the store, whatever it says.
    NextRead,
    /// The agent reporting something other than this many events.
    NewEvents(u64),
}

#[derive(Debug, Clone, Copy)]
pub enum Phase {
    /// Nothing is with the worker: ask when the agent is ready.
    Waiting,
    /// The first message is with the worker.
    InFlight,
    /// The worker found the agent held or unreadable: wait for fresh
    /// activity before asking again.
    Refused { release: Release, at: Instant },
}

/// What's queued to type into one agent, oldest first. It exists only
/// while something is queued.
#[derive(Debug)]
pub struct Outbox {
    first: Queued,
    rest: VecDeque<Queued>,
    phase: Phase,
}

impl Outbox {
    pub fn new(prompt: Prompt, comments: Vec<crate::review::Comment>) -> Self {
        Self {
            first: Queued { prompt, comments },
            rest: VecDeque::new(),
            phase: Phase::Waiting,
        }
    }

    pub fn push(&mut self, prompt: Prompt, comments: Vec<crate::review::Comment>) {
        self.rest.push_back(Queued { prompt, comments });
    }

    #[cfg(test)]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn in_flight(&self) -> bool {
        matches!(self.phase, Phase::InFlight)
    }

    /// Whether the worker may be asked to type the first message, as far as
    /// a refusal goes.
    pub fn askable(&self, now: Instant) -> bool {
        match self.phase {
            Phase::Waiting => true,
            Phase::InFlight => false,
            Phase::Refused { at, .. } => now.saturating_duration_since(at) > REFUSED_FOR,
        }
    }

    pub fn front(&self) -> &Prompt {
        &self.first.prompt
    }

    pub fn len(&self) -> usize {
        1 + self.rest.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Prompt> {
        std::iter::once(&self.first)
            .chain(&self.rest)
            .map(|q| &q.prompt)
    }

    /// Every queued message's comments, oldest first.
    pub fn into_comments(self) -> Vec<crate::review::Comment> {
        std::iter::once(self.first)
            .chain(self.rest)
            .flat_map(|q| q.comments)
            .collect()
    }

    /// The first message goes to the worker.
    pub fn start(&mut self) -> Prompt {
        self.phase = Phase::InFlight;
        self.first.prompt.clone()
    }

    /// The first message was typed: what's left, if anything.
    pub fn delivered(mut self) -> Option<Self> {
        self.phase = Phase::Waiting;
        self.rest.pop_front().map(|first| Self { first, ..self })
    }

    /// The first message was refused for now.
    pub fn refused(&mut self, release: Release, at: Instant) {
        self.phase = Phase::Refused { release, at };
    }

    /// The first message can't be sent: it, and what's left if anything.
    pub fn dropped(mut self) -> (Queued, Option<Self>) {
        self.phase = Phase::Waiting;
        match self.rest.pop_front() {
            Some(next) => {
                let dropped = std::mem::replace(&mut self.first, next);
                (dropped, Some(self))
            }
            None => (self.first, None),
        }
    }

    /// Lifts the refusal if a read of the store, with the agent's event count
    /// in it, answers it.
    pub fn read(&mut self, events: Option<u64>) {
        if let Phase::Refused { release, .. } = self.phase
            && match release {
                Release::NextRead => true,
                Release::NewEvents(held) => events != Some(held),
            }
        {
            self.phase = Phase::Waiting;
        }
    }
}

/// An outbox holding one message, for tests.
#[cfg(test)]
pub fn queued(text: &str) -> Outbox {
    Outbox::new(text.parse().expect("a prompt"), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbox(texts: &[&str]) -> Outbox {
        let mut outbox = queued(texts[0]);
        for text in &texts[1..] {
            outbox.push(text.parse().expect("a prompt"), Vec::new());
        }
        outbox
    }

    #[test]
    fn a_queue_starts_waiting_goes_in_flight_and_waits_again_once_the_first_is_typed() {
        let now = Instant::now();
        let mut outbox = outbox(&["one", "two"]);
        assert!(matches!(outbox.phase(), Phase::Waiting));
        assert!(outbox.askable(now));
        assert_eq!(outbox.start().as_str(), "one");
        assert!(outbox.in_flight() && !outbox.askable(now));

        let outbox = outbox.delivered().expect("one is left");
        assert!(matches!(outbox.phase(), Phase::Waiting));
        assert_eq!(outbox.front().as_str(), "two");
        assert_eq!(outbox.len(), 1);
        assert!(outbox.delivered().is_none(), "emptied");
    }

    #[test]
    fn a_refusal_lasts_until_its_release_or_its_time_runs_out() {
        let now = Instant::now();
        let mut outbox = outbox(&["one"]);
        outbox.start();
        outbox.refused(Release::NewEvents(5), now);
        assert!(matches!(outbox.phase(), Phase::Refused { .. }));
        assert!(!outbox.askable(now));

        outbox.read(Some(5));
        assert!(
            matches!(outbox.phase(), Phase::Refused { .. }),
            "nothing new"
        );
        outbox.read(None);
        assert!(matches!(outbox.phase(), Phase::Waiting), "it changed");

        outbox.refused(Release::NextRead, now);
        outbox.read(Some(5));
        assert!(
            matches!(outbox.phase(), Phase::Waiting),
            "any read lifts it"
        );

        outbox.refused(Release::NewEvents(5), now);
        assert!(
            !outbox.askable(now + REFUSED_FOR),
            "refused for exactly its time"
        );
        assert!(
            outbox.askable(now + REFUSED_FOR + Duration::from_secs(1)),
            "asked again after its time"
        );
    }

    #[test]
    fn a_dropped_message_leaves_the_rest_waiting() {
        let mut outbox = outbox(&["one", "two"]);
        outbox.start();
        let (dropped, rest) = outbox.dropped();
        assert_eq!(dropped.prompt.as_str(), "one");
        let rest = rest.expect("two is left");
        assert!(matches!(rest.phase(), Phase::Waiting));
        assert_eq!(rest.front().as_str(), "two");
        assert!(rest.dropped().1.is_none());
    }
}
