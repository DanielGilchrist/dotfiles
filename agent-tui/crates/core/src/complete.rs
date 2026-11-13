//! What a shell offers for a word of a command line. Everything here reads
//! the cheapest source that is accurate enough: directory listings, the
//! store's files, the host's session list and git refs. A full snapshot is
//! never built, so a completion stays well under a keystroke's patience.

use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Display};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::config::Sources;
use crate::discover;
use crate::names::SessionName;
use crate::ops::{self, Services};
use crate::ports::Liveness;

/// Text that fits one tab-separated line of a completion listing: no
/// control character, so it can never be split into two candidates or
/// start a description.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Word(String);

impl Word {
    /// `text` as it is, or nothing when it holds a control character. A
    /// candidate that can't be offered exactly isn't offered.
    pub fn exact(text: &str) -> Option<Self> {
        (!text.chars().any(char::is_control)).then(|| Self(text.into()))
    }

    /// `text` with each control character read as a space, for a note.
    pub fn describing(text: &str) -> Self {
        Self(
            text.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a candidate's line carries its note, for the shells that show one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notes {
    Shown,
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    word: Word,
    note: Option<Word>,
}

impl Candidate {
    pub fn new(word: Word, note: Option<Word>) -> Self {
        Self { word, note }
    }

    pub fn word(&self) -> &Word {
        &self.word
    }

    pub fn note(&self) -> Option<&Word> {
        self.note.as_ref()
    }

    pub fn line(&self, notes: Notes) -> String {
        match (notes, &self.note) {
            (Notes::Shown, Some(note)) => format!("{}\t{note}", self.word),
            _ => self.word.to_string(),
        }
    }
}

/// What a completion found, in the order to offer it, and what it couldn't
/// read. A source that fails costs its own candidates and no others.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Completion {
    pub candidates: Vec<Candidate>,
    pub problems: Vec<String>,
}

impl Completion {
    fn settle<T: Default, E: Display>(&mut self, result: Result<T, E>) -> T {
        result.unwrap_or_else(|e| {
            self.problems.push(e.to_string());
            T::default()
        })
    }
}

/// How an agent stands, in the order it is offered. Without the host's
/// listing nothing is known, and that comes right after the live ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    Live,
    Unknown,
    Exited,
    NoSession,
}

impl Standing {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Unknown => "unknown",
            Self::Exited => "exited",
            Self::NoSession => "no session",
        }
    }
}

/// How long a completion waits on any one source before going without it,
/// so a hung mount or session host can't hold up a prompt.
pub const SOURCE_BUDGET: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
#[error("{what} took longer than {}ms, so it was left out", .limit.as_millis())]
pub struct Slow {
    what: &'static str,
    limit: Duration,
}

/// `work` on a thread of its own, abandoned if it hasn't answered within
/// `limit`. A read of a hung mount can't be interrupted, so the thread is
/// left behind and ends with the process.
pub fn bounded<T: Send + 'static>(
    what: &'static str,
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Slow> {
    let (answer, heard) = mpsc::channel();
    thread::spawn(move || drop(answer.send(work())));
    heard.recv_timeout(limit).map_err(|_| Slow { what, limit })
}

/// The agents this tool knows, as `agent attach` can reach them: worktrees
/// under the worktrees root, the main-checkout sessions of listed repos and
/// every stored record. The host's sessions only say whether those are
/// live. A session that matches none of them isn't an agent of ours and is
/// left out. Live agents come first.
pub fn agents(svc: Services<'_>) -> Completion {
    agents_within(svc, SOURCE_BUDGET)
}

fn agents_within(svc: Services<'_>, budget: Duration) -> Completion {
    let mut done = Completion::default();
    let mut known: BTreeMap<SessionName, Option<String>> = BTreeMap::new();

    let layout = svc.layout.clone();
    let scanned = bounded("reading the worktrees", budget, move || layout.scan())
        .map_err(|slow| slow.to_string());
    let worktrees = done.settle(scanned.and_then(|r| r.map_err(|e| e.to_string())));
    for worktree in worktrees {
        let repo = worktree
            .path()
            .parent()
            .and_then(Path::file_name)
            .map(|dir| dir.to_string_lossy().into_owned());
        known.insert(worktree.name().session(), repo);
    }
    for repo in done.settle(svc.store.repos()) {
        let name = repo.name().into_owned();
        if let Ok(session) = name.parse() {
            known.entry(session).or_insert(Some(name));
        }
    }
    for name in done.settle(svc.store.recorded()) {
        known.entry(name).or_insert(None);
    }

    let liveness: Option<HashMap<SessionName, Liveness>> = match svc.host.liveness(budget) {
        Ok(listed) => Some(listed.into_iter().collect()),
        Err(e) => {
            done.problems.push(e.to_string());
            None
        }
    };

    let mut ranked: Vec<_> = known
        .into_iter()
        .filter_map(|(name, repo)| {
            let standing = match &liveness {
                None => Standing::Unknown,
                Some(listed) => match listed.get(&name) {
                    Some(Liveness::Live) => Standing::Live,
                    Some(Liveness::Exited) => Standing::Exited,
                    None => Standing::NoSession,
                },
            };
            let note = match repo {
                Some(repo) => format!("{} · {repo}", standing.label()),
                None => standing.label().to_string(),
            };
            let word = Word::exact(name.as_str())?;
            Some((
                standing,
                Candidate::new(word, Some(Word::describing(&note))),
            ))
        })
        .collect();
    ranked.sort_by_key(|(standing, _)| *standing);
    done.candidates = ranked.into_iter().map(|(_, c)| c).collect();
    done
}

/// The branches of the repo containing `dir`: local ones, then those only
/// on origin. Outside a repo there are none.
pub fn branches(svc: Services<'_>, dir: &Path) -> Completion {
    let mut done = Completion::default();
    let Some(repo) = done.settle(ops::repo_for(svc, dir)) else {
        return done;
    };
    let Some(found) = done.settle(svc.vcs.branches(&repo).map(Some)) else {
        return done;
    };
    let local = found.local.iter().map(|b| (b, "local"));
    let remote = found
        .on_origin
        .iter()
        .filter(|b| !found.local.contains(b))
        .map(|b| (b, "origin"));
    done.candidates = local
        .chain(remote)
        .filter_map(|(branch, note)| {
            let word = Word::exact(branch.as_str())?;
            Some(Candidate::new(word, Some(Word::describing(note))))
        })
        .collect();
    done
}

/// The listed repos, then the ones `sources` finds that aren't listed yet.
pub fn repos(svc: Services<'_>, sources: &Sources) -> Completion {
    let mut done = Completion::default();
    let listed = done.settle(svc.store.repos());
    let dirs = sources.dirs().to_vec();
    let found = bounded("reading the repo directories", SOURCE_BUDGET, move || {
        discover::catalogue(&dirs)
    });
    let found = done.settle(found.map(Some)).unwrap_or_default();
    let offered = listed
        .iter()
        .map(|repo| (repo, "listed"))
        .chain(found.repos.iter().map(|repo| (repo, "found")));
    let mut seen = Vec::new();
    for (repo, note) in offered {
        let path = repo.path();
        if seen.contains(&path) {
            continue;
        }
        seen.push(path);
        let word = path.to_str().and_then(Word::exact);
        done.candidates
            .extend(word.map(|word| Candidate::new(word, Some(Word::describing(note)))));
    }
    done
}

/// The ids the registry knows.
pub fn harnesses(svc: Services<'_>) -> Completion {
    let candidates = svc
        .harnesses
        .all()
        .iter()
        .filter_map(|harness| Word::exact(&harness.id().to_string()))
        .map(|word| Candidate::new(word, None))
        .collect();
    Completion {
        candidates,
        problems: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Head;
    use crate::names::AgentName;
    use crate::ports::{AgentRecord, Branches, SessionInfo, SessionStatus, Store};
    use crate::testing::World;

    fn lines(done: &Completion, notes: Notes) -> Vec<String> {
        done.candidates.iter().map(|c| c.line(notes)).collect()
    }

    fn session(s: &str) -> SessionName {
        s.parse().expect("parses")
    }

    fn add_worktree(w: &World, name: &str) {
        let agent: AgentName = name.parse().expect("parses");
        let at = w.layout.worktree(&w.vcs.root, agent);
        std::fs::create_dir_all(at.path().join(".git")).expect("creates");
        w.vcs.set_head(at.path(), Head::Detached);
    }

    fn host_session(w: &World, name: &str, status: SessionStatus) {
        w.host.sessions.lock().expect("lock").push(SessionInfo {
            name: session(name),
            status,
        });
    }

    #[test]
    fn a_word_with_a_control_character_is_never_offered_whole() {
        assert!(Word::exact("a\tb").is_none());
        assert!(Word::exact("a\nb").is_none());
        assert_eq!(Word::exact("a b").expect("fits").as_str(), "a b");
        assert_eq!(Word::describing("a\tb\nc").as_str(), "a b c");
    }

    #[test]
    fn a_line_carries_its_note_only_where_notes_are_shown() {
        let c = Candidate::new(
            Word::exact("fix").expect("fits"),
            Some(Word::describing("live · api")),
        );
        assert_eq!(c.line(Notes::Shown), "fix\tlive · api");
        assert_eq!(c.line(Notes::Hidden), "fix");
    }

    #[test]
    fn live_agents_come_first_marked_then_the_rest_with_their_state() {
        let w = World::new();
        for name in ["alpha", "bravo", "charlie", "delta"] {
            add_worktree(&w, name);
        }
        host_session(&w, "delta", SessionStatus::Live { cwd: None });
        host_session(&w, "bravo", SessionStatus::Exited { cwd: None });
        let done = agents(w.svc());
        let root = w.vcs.root.name().into_owned();
        assert_eq!(
            lines(&done, Notes::Shown),
            vec![
                format!("delta\tlive · {root}"),
                format!("bravo\texited · {root}"),
                format!("alpha\tno session · {root}"),
                format!("charlie\tno session · {root}"),
            ]
        );
        assert_eq!(done.problems, Vec::<String>::new());
    }

    #[test]
    fn without_the_hosts_listing_every_agent_is_unknown() {
        let w = World::new();
        add_worktree(&w, "alpha");
        host_session(&w, "alpha", SessionStatus::Live { cwd: None });
        *w.host.liveness_fails.lock().expect("lock") = true;
        let done = agents(w.svc());
        assert_eq!(done.candidates.len(), 1);
        assert!(
            done.candidates[0]
                .note()
                .expect("noted")
                .as_str()
                .starts_with("unknown")
        );
        assert_eq!(done.problems.len(), 1);
    }

    #[test]
    fn only_agents_the_tool_knows_are_offered_and_the_host_only_sets_their_state() {
        let w = World::new();
        w.store.pin(&session("pinned")).expect("pins");
        w.store
            .put_record(
                &session("recorded"),
                &AgentRecord::new("claude".parse().expect("parses"), None),
            )
            .expect("puts");
        w.store.list_repo(&w.vcs.root).expect("lists");
        host_session(&w, "stray", SessionStatus::Exited { cwd: None });
        host_session(&w, "recorded", SessionStatus::Live { cwd: None });
        let done = agents(w.svc());
        let repo = w.vcs.root.name().into_owned();
        assert_eq!(
            lines(&done, Notes::Shown),
            [
                "recorded\tlive".to_string(),
                format!("{repo}\tno session · {repo}"),
            ]
        );
    }

    #[test]
    fn a_source_that_overruns_its_budget_is_left_out_with_a_problem() {
        let slow = bounded("reading things", Duration::from_millis(20), || {
            thread::sleep(Duration::from_secs(5));
        });
        assert!(slow.is_err());
        let fast = bounded("reading things", Duration::from_secs(5), || 7);
        assert_eq!(fast.expect("answers"), 7);
        let message = bounded("reading things", Duration::from_millis(20), || {
            thread::sleep(Duration::from_secs(5));
        })
        .expect_err("slow")
        .to_string();
        assert_eq!(
            message,
            "reading things took longer than 20ms, so it was left out"
        );
    }

    #[test]
    fn local_branches_come_before_those_only_on_origin() {
        let w = World::new();
        *w.vcs.branch_names.lock().expect("lock") = Branches {
            local: vec![
                "main".parse().expect("parses"),
                "fix".parse().expect("parses"),
            ],
            on_origin: vec![
                "main".parse().expect("parses"),
                "feature/x".parse().expect("parses"),
            ],
        };
        let done = branches(w.svc(), w.layout.root());
        assert_eq!(
            lines(&done, Notes::Shown),
            ["main\tlocal", "fix\tlocal", "feature/x\torigin"]
        );
    }

    #[test]
    fn listed_repos_come_before_found_ones_and_appear_once() {
        let w = World::new();
        let tmp = tempfile::tempdir().expect("a temp dir");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(clone.join(".git")).expect("creates");
        let sources = Sources::new(vec![tmp.path().to_path_buf()]);
        w.store.list_repo(&w.vcs.root).expect("lists");
        let real = clone.canonicalize().expect("exists");
        let done = repos(w.svc(), &sources);
        assert_eq!(
            lines(&done, Notes::Shown),
            [
                format!("{}\tlisted", w.vcs.root.path().display()),
                format!("{}\tfound", real.display()),
            ]
        );
    }
}
