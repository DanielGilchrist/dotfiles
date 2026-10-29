use std::borrow::Cow;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use crate::names::{BranchName, SessionName};
use crate::paths::{AgentWorktree, RepoRoot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    Branch(BranchName),
    Detached,
}

impl Head {
    pub fn branch(&self) -> Option<&BranchName> {
        match self {
            Self::Branch(branch) => Some(branch),
            Self::Detached => None,
        }
    }

    pub fn into_branch(self) -> Option<BranchName> {
        match self {
            Self::Branch(branch) => Some(branch),
            Self::Detached => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Head,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Presence {
    Live,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkout {
    Main(RepoRoot),
    Agent(AgentWorktree),
    /// A linked worktree outside the agent layout.
    External(PathBuf),
}

impl Checkout {
    pub fn path(&self) -> &Path {
        match self {
            Self::Main(root) => root.path(),
            Self::Agent(wt) => wt.path(),
            Self::External(path) => path,
        }
    }
}

/// How an entry relates to repos and sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A repo's main checkout, named after the repo.
    Main {
        root: RepoRoot,
        head: Head,
        session: Option<Presence>,
    },
    /// An agent worktree, named after its directory.
    Agent {
        worktree: AgentWorktree,
        head: Head,
        session: Option<Presence>,
    },
    /// A session with its own name whose terminal is inside a known checkout.
    Adopted {
        checkout: Checkout,
        head: Head,
        presence: Presence,
    },
    /// A session inside no repo we know of.
    Orphan {
        cwd: Option<PathBuf>,
        presence: Presence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Start {
    /// A repo's main checkout; the name is the repo's.
    Main {
        name: SessionName,
        root: RepoRoot,
    },
    Agent(AgentWorktree),
}

/// Somewhere a new session can be started: an idle main checkout or agent
/// worktree, with the session name that place implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Startable(Start);

/// An entry's session, or where one can start when there's none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Live,
    /// Dead but resurrectable.
    Exited(Revival),
    Idle(Startable),
}

/// How an exited session comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revival {
    /// One of our checkouts: start it again through `agent exec`, which
    /// resumes the conversation with today's launch (hooks included),
    /// whatever command zellij cached for it.
    Restart(Startable),
    /// A session we don't own the checkout of: zellij replays its cached
    /// command.
    Resurrect,
}

impl Startable {
    pub(crate) fn worktree(worktree: &AgentWorktree) -> Self {
        Self(Start::Agent(worktree.clone()))
    }

    /// A repo's main checkout, in a session named after the repo.
    pub(crate) fn main(root: &RepoRoot) -> Result<Self, crate::names::NameError> {
        Ok(Self(Start::Main {
            name: root.name().parse()?,
            root: root.clone(),
        }))
    }

    pub fn name(&self) -> SessionName {
        match &self.0 {
            Start::Main { name, .. } => name.clone(),
            Start::Agent(worktree) => worktree.name().session(),
        }
    }

    pub fn cwd(&self) -> &Path {
        match &self.0 {
            Start::Main { root, .. } => root.path(),
            Start::Agent(worktree) => worktree.path(),
        }
    }
}

/// What removing an entry would act on, decided from the entry itself so a
/// same-named agent in another repo can't be hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removable {
    Worktree(AgentWorktree),
    /// A session with no agent worktree (a main checkout, an adopted or
    /// orphan session): only the session goes.
    Session(SessionName),
}

/// A jump key, 1 through 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slot(u8);

impl Slot {
    pub fn new(n: u8) -> Option<Self> {
        (1..=9).contains(&n).then_some(Self(n))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// Position in the user's pin list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pin(usize);

impl Pin {
    /// The first nine pins get jump slots.
    pub fn slot(self) -> Option<Slot> {
        u8::try_from(self.0 + 1).ok().and_then(Slot::new)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    name: SessionName,
    kind: EntryKind,
    pin: Option<Pin>,
}

/// A session seen running that isn't the caller's own: the only thing a
/// kill accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running(SessionName);

impl Running {
    pub(crate) fn new(name: SessionName) -> Self {
        Self(name)
    }

    pub fn name(&self) -> &SessionName {
        &self.0
    }
}

impl Entry {
    /// Its session, if it's running and isn't `own`.
    pub fn running(&self, own: Option<&SessionName>) -> Option<Running> {
        match self.presence() {
            Some(Presence::Live) if own != Some(&self.name) => Some(Running(self.name.clone())),
            _ => None,
        }
    }

    pub(crate) fn new(name: SessionName, kind: EntryKind) -> Self {
        Self {
            name,
            kind,
            pin: None,
        }
    }

    pub(crate) fn pin_at(&mut self, position: usize) {
        self.pin = Some(Pin(position));
    }

    pub fn name(&self) -> &SessionName {
        &self.name
    }

    pub fn kind(&self) -> &EntryKind {
        &self.kind
    }

    pub fn pin(&self) -> Option<Pin> {
        self.pin
    }

    pub fn presence(&self) -> Option<Presence> {
        match &self.kind {
            EntryKind::Main { session, .. } | EntryKind::Agent { session, .. } => *session,
            EntryKind::Adopted { presence, .. } | EntryKind::Orphan { presence, .. } => {
                Some(*presence)
            }
        }
    }

    pub fn head(&self) -> Option<&Head> {
        match &self.kind {
            EntryKind::Main { head, .. }
            | EntryKind::Agent { head, .. }
            | EntryKind::Adopted { head, .. } => Some(head),
            EntryKind::Orphan { .. } => None,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.kind {
            EntryKind::Main { root, .. } => Some(root.path()),
            EntryKind::Agent { worktree, .. } => Some(worktree.path()),
            EntryKind::Adopted { checkout, .. } => Some(checkout.path()),
            EntryKind::Orphan { cwd, .. } => cwd.as_deref(),
        }
    }

    pub fn is_main(&self) -> bool {
        matches!(self.kind, EntryKind::Main { .. })
    }

    /// The agent worktree this entry owns (and removing it would delete).
    pub fn worktree(&self) -> Option<&AgentWorktree> {
        match &self.kind {
            EntryKind::Agent { worktree, .. } => Some(worktree),
            _ => None,
        }
    }

    pub fn removable(&self) -> Option<Removable> {
        match (&self.kind, self.state()) {
            (EntryKind::Agent { worktree, .. }, _) => Some(Removable::Worktree(worktree.clone())),
            (_, EntryState::Live | EntryState::Exited(_)) => {
                Some(Removable::Session(self.name.clone()))
            }
            (_, EntryState::Idle(_)) => None,
        }
    }

    /// Its session, or where one can start when there's none.
    pub fn state(&self) -> EntryState {
        let main = |root: &RepoRoot| {
            Startable(Start::Main {
                name: self.name.clone(),
                root: root.clone(),
            })
        };
        match &self.kind {
            EntryKind::Main {
                session: Some(Presence::Live),
                ..
            }
            | EntryKind::Agent {
                session: Some(Presence::Live),
                ..
            }
            | EntryKind::Adopted {
                presence: Presence::Live,
                ..
            }
            | EntryKind::Orphan {
                presence: Presence::Live,
                ..
            } => EntryState::Live,
            EntryKind::Main {
                root,
                session: Some(Presence::Exited),
                ..
            } => EntryState::Exited(Revival::Restart(main(root))),
            EntryKind::Agent {
                worktree,
                session: Some(Presence::Exited),
                ..
            } => EntryState::Exited(Revival::Restart(Startable::worktree(worktree))),
            EntryKind::Adopted {
                presence: Presence::Exited,
                ..
            }
            | EntryKind::Orphan {
                presence: Presence::Exited,
                ..
            } => EntryState::Exited(Revival::Resurrect),
            EntryKind::Main {
                root,
                session: None,
                ..
            } => EntryState::Idle(main(root)),
            EntryKind::Agent {
                worktree,
                session: None,
                ..
            } => EntryState::Idle(Startable::worktree(worktree)),
        }
    }

    /// The main checkout reads best as its branch; everything else by name.
    pub fn label(&self) -> &str {
        match &self.kind {
            EntryKind::Main {
                head: Head::Branch(branch),
                ..
            } => branch.as_str(),
            _ => self.name.as_str(),
        }
    }

    /// Pinned first (in pin order), then the main checkout, then live,
    /// exited and idle entries, then by name.
    fn sort_key(&self) -> (bool, Option<Pin>, bool, u8, String) {
        let state = match self.presence() {
            Some(Presence::Live) => 0,
            Some(Presence::Exited) => 1,
            None => 2,
        };
        (
            self.pin.is_none(),
            self.pin,
            !self.is_main(),
            state,
            self.name.as_str().to_lowercase(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RepoKey {
    Root(RepoRoot),
    /// A session that doesn't belong to any repo we know of.
    Orphan(SessionName),
}

impl RepoKey {
    pub fn name(&self) -> Cow<'_, str> {
        match self {
            Self::Root(root) => root.name(),
            Self::Orphan(name) => Cow::Borrowed(name.as_str()),
        }
    }

    pub fn root(&self) -> Option<&RepoRoot> {
        match self {
            Self::Root(root) => Some(root),
            Self::Orphan(_) => None,
        }
    }

    fn rank(&self) -> (String, u8, &Path, &str) {
        match self {
            Self::Root(root) => (root.name().to_lowercase(), 0, root.path(), ""),
            Self::Orphan(name) => (
                name.as_str().to_lowercase(),
                1,
                Path::new(""),
                name.as_str(),
            ),
        }
    }
}

impl Ord for RepoKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for RepoKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub live: usize,
    pub exited: usize,
    pub idle: usize,
}

/// A repo and its entries; never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    key: RepoKey,
    entries: Vec<Entry>,
}

impl Repo {
    pub(crate) fn new(key: RepoKey, mut entries: Vec<Entry>) -> Option<Self> {
        if entries.is_empty() {
            return None;
        }
        entries.sort_by_cached_key(Entry::sort_key);
        Some(Self { key, entries })
    }

    pub fn key(&self) -> &RepoKey {
        &self.key
    }

    pub fn name(&self) -> Cow<'_, str> {
        self.key.name()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn first(&self) -> &Entry {
        &self.entries[0]
    }

    pub fn entry(&self, name: &SessionName) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.name == name)
    }

    pub fn counts(&self) -> Counts {
        self.entries.iter().fold(Counts::default(), |mut c, e| {
            match e.presence() {
                Some(Presence::Live) => c.live += 1,
                Some(Presence::Exited) => c.exited += 1,
                None => c.idle += 1,
            }
            c
        })
    }
}

/// Every repo and entry, sorted; repo keys and session names are unique.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    repos: Vec<Repo>,
}

impl Snapshot {
    pub(crate) fn new(mut repos: Vec<Repo>) -> Self {
        repos.sort_by(|a, b| a.key.cmp(&b.key));
        Self { repos }
    }

    pub fn repos(&self) -> &[Repo] {
        &self.repos
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    pub fn repo(&self, key: &RepoKey) -> Option<&Repo> {
        self.repos.iter().find(|r| &r.key == key)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&Repo, &Entry)> {
        self.repos
            .iter()
            .flat_map(|r| r.entries.iter().map(move |e| (r, e)))
    }

    pub fn entry(&self, name: &SessionName) -> Option<(&Repo, &Entry)> {
        self.entries().find(|(_, e)| &e.name == name)
    }

    /// Pinned entries in pin order.
    pub fn pinned(&self) -> Vec<(&Repo, &Entry)> {
        let mut pinned: Vec<_> = self.entries().filter(|(_, e)| e.pin.is_some()).collect();
        pinned.sort_by_key(|(_, e)| e.pin);
        pinned
    }

    pub fn slot(&self, slot: Slot) -> Option<(&Repo, &Entry)> {
        self.entries()
            .find(|(_, e)| e.pin.and_then(Pin::slot) == Some(slot))
    }

    /// The deepest entry whose checkout contains `path`.
    pub fn containing(&self, path: &Path) -> Option<(&Repo, &Entry)> {
        self.entries()
            .filter(|(_, e)| e.path().is_some_and(|p| path.starts_with(p)))
            .max_by_key(|(_, e)| e.path().map_or(0, |p| p.as_os_str().len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_one_through_nine() {
        assert!(Slot::new(0).is_none());
        assert!(Slot::new(10).is_none());
        assert_eq!(Pin(0).slot(), Slot::new(1));
        assert_eq!(Pin(8).slot(), Slot::new(9));
        assert!(Pin(9).slot().is_none());
    }

    #[test]
    fn orphan_keys_differing_only_in_case_are_ordered_not_equal() {
        let a = RepoKey::Orphan("Api".parse().unwrap());
        let b = RepoKey::Orphan("api".parse().unwrap());
        assert_ne!(a.cmp(&b), Ordering::Equal);
    }
}
