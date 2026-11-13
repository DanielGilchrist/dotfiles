//! What the model remembers per agent and per subagent, each kept in one
//! record so pruning one drops everything known about it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use agent_core::activity::{AgentId, Standing, Subagent};
use agent_core::ports::{ActivityDetail, Log, TranscriptStamp, WorktreeFacts};

use crate::cause::Cause;
use crate::log::Nav;
use crate::model::{Company, Inspection, Ticket};
use crate::touched::Baseline;
use crate::tree::{ChangesScope, ChangesView, TreeRow};
use crate::typed::Message;

/// Which read of an agent's git facts something was made from. Reads are
/// counted across all agents, so an epoch never repeats for an agent that
/// leaves and comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactsEpoch(u64);

impl FactsEpoch {
    /// Before any read.
    pub const ORIGIN: Self = Self(0);

    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// One read of an agent's git facts and what it found.
pub struct FactsRead {
    pub epoch: FactsEpoch,
    pub result: Result<Inspected, Cause>,
}

impl Compose {
    /// Whether dropping it would lose something: text, or a message the
    /// worker may or may not have typed.
    pub fn holds_work(&self) -> bool {
        match self {
            Self::Editing { text, .. } => !text.is_blank(),
            Self::Sending { .. } => true,
        }
    }
}

/// The tree built for an agent, rebuilt only when its facts, the view or
/// its closed folders change.
pub struct CachedTree {
    /// The facts it was built from, none while there were none.
    pub epoch: Option<FactsEpoch>,
    pub view: ChangesView,
    pub scope: ChangesScope,
    pub collapsed: HashSet<PathBuf>,
    pub rows: Arc<Vec<TreeRow>>,
}

/// What the user has typed for an agent that hasn't been typed into it.
pub enum Compose {
    Editing {
        quote: Option<crate::log::Quote>,
        text: Message,
    },
    /// With the worker, which answers for `ticket`. The comments are the
    /// review it carries, given back to the draft if it isn't typed.
    Sending {
        quote: Option<crate::log::Quote>,
        text: Message,
        comments: Vec<crate::review::Comment>,
        ticket: Ticket,
    },
}

/// Everything the model keeps for one agent, dropped together when the
/// agent leaves the snapshot.
#[derive(Default)]
pub struct Tracked {
    /// Its git facts, or why they couldn't be read, as last read.
    pub facts: Option<FactsRead>,
    /// The folders its [3] has closed. A folder that isn't named is open,
    /// so one that appears starts open.
    pub collapsed: HashSet<PathBuf>,
    pub tree: Option<CachedTree>,
    /// When its facts were last asked for.
    pub inspection: Option<Inspection>,
    /// Comments drafted on diffs, until sent.
    pub draft: Vec<crate::review::Comment>,
    /// What is being written to it, or sent and not yet answered. It stays
    /// across the box closing, and goes once the message is typed.
    pub compose: Option<Compose>,
    /// Whether a snapshot has listed it, and when this TUI last started or
    /// resurrected its session.
    pub seen: Seen,
    /// The subagents being watched, dropped with the agent.
    pub subagents: HashMap<AgentId, Watched>,
    /// The command [2]'s cursor is on, as last listed.
    pub held_command: Option<crate::model::HeldCommand>,
    /// The other windows showing its session, as the newest look found them.
    pub company: Company,
    /// Its current activity, as [0] last read it.
    pub activity_view: ActivityWatch,
    /// Where reading its transcript for an interrupt stands.
    pub probed: Option<Probed>,
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
    pub log: Option<Result<Option<T>, Cause>>,
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
    Listed,
}

impl Seen {
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
            Seen::Listed => Seen::Listed,
            Seen::Unlaunched | Seen::Pending { .. } => Seen::Pending { launched_at: at },
        };
    }

    /// A snapshot lists it.
    pub fn listed(&mut self) {
        self.seen = Seen::Listed;
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
