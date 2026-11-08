//! What the main view [0] reads when it isn't the terminal: documents
//! loaded off the UI thread, a cache of the last few, and the scroll
//! position of the one on screen.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::activity::AgentId;
use agent_core::model::RepoKey;
use agent_core::names::SessionName;
use agent_core::ports::{BranchLog, DiffBase, FileDiff, Lines};
use ratatui::text::Line;

/// The cursor must rest on a file this long before its diff is asked for, so
/// holding `j` doesn't start a read per file.
pub const REST: Duration = Duration::from_millis(60);
/// A document that takes longer than this shows `loading`.
pub const LOADING_AFTER: Duration = Duration::from_millis(150);
/// Documents kept, the least recently shown going first.
const CACHE: usize = 32;
/// And at most this much text, so a few huge diffs can't hold the memory.
const BUDGET: usize = 8 * 1024 * 1024;
/// A read that failed is asked again after this long.
const ERROR_TTL: Duration = Duration::from_secs(5);

/// A changed file's diff: whose it is, which file, and the numstat line it
/// was read at, so a file that changed again is a different document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffKey {
    pub name: SessionName,
    pub path: PathBuf,
    pub lines: Lines,
    /// What the diff is read against, so the same file in the other view
    /// of [3] is another document.
    pub base: DiffBase,
    /// Counts the agent's git facts read, so a file edited without its
    /// counts changing is a different document.
    pub generation: u64,
}

/// A folder in [3]: the files under it are listed from the git facts, so
/// nothing is read for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderKey {
    pub name: SessionName,
    pub path: PathBuf,
    pub generation: u64,
}

/// A branch's log: whose it is and the counts it was read at, so a branch
/// that gained a commit or a dirty file is a different document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchKey {
    pub name: SessionName,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: usize,
    /// As in [`DiffKey`], so a swapped commit shows.
    pub generation: u64,
}

/// A document read off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Diff(DiffKey),
    Branch(BranchKey),
}

/// What one read answers: an agent's file, or its branch. A newer read for
/// the same slot replaces an older one that hasn't started.
pub type Slot = (SessionName, Option<PathBuf>);

impl Key {
    pub fn slot(&self) -> Slot {
        match self {
            Self::Diff(key) => (key.name.clone(), Some(key.path.clone())),
            Self::Branch(key) => (key.name.clone(), None),
        }
    }

    pub fn name(&self) -> &SessionName {
        match self {
            Self::Diff(key) => &key.name,
            Self::Branch(key) => &key.name,
        }
    }
}

/// What a read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Doc {
    Diff(FileDiff),
    Branch(BranchLog),
}

impl Doc {
    /// Roughly how much memory it holds.
    fn bytes(&self) -> usize {
        match self {
            Self::Diff(FileDiff::Text { lines, .. }) => lines.iter().map(String::len).sum(),
            Self::Diff(FileDiff::Binary { .. }) => 0,
            Self::Branch(log) => {
                log.commits.iter().map(String::len).sum::<usize>() + log.uncommitted.len() * 64
            }
        }
    }
}

/// What the cursor is on that [0] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hover {
    /// A changed file in [3].
    Diff(DiffKey),
    /// [3]'s readiness row.
    Branch(BranchKey),
    /// A folder in [3].
    Folder(FolderKey),
    /// [2]'s turn row.
    Turn(SessionName),
    /// A subagent row in [2].
    Subagent(SessionName, AgentId),
    /// A repo row in [1]: nothing to read, but a summary to show.
    Repo(RepoKey),
}

/// What a scroll position belongs to, so another item starts afresh.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    File(SessionName, PathBuf),
    Branch(SessionName),
    Folder(SessionName, PathBuf),
    Turn(SessionName),
    Subagent(SessionName, AgentId),
    Repo(RepoKey),
}

impl Hover {
    fn item(&self) -> Item {
        match self {
            Self::Diff(key) => Item::File(key.name.clone(), key.path.clone()),
            Self::Branch(key) => Item::Branch(key.name.clone()),
            Self::Folder(key) => Item::Folder(key.name.clone(), key.path.clone()),
            Self::Turn(name) => Item::Turn(name.clone()),
            Self::Subagent(name, id) => Item::Subagent(name.clone(), id.clone()),
            Self::Repo(key) => Item::Repo(key.clone()),
        }
    }

    /// The document a read has to produce for this, if it needs one.
    fn key(&self) -> Option<Key> {
        match self {
            Self::Diff(key) => Some(Key::Diff(key.clone())),
            Self::Branch(key) => Some(Key::Branch(key.clone())),
            Self::Turn(_) | Self::Subagent(..) | Self::Repo(_) | Self::Folder(_) => None,
        }
    }
}

pub type Loaded = Arc<Result<Doc, String>>;

/// The document the cursor is on, since when, and since when [0] has had
/// nothing current to show for it.
struct Wanted {
    key: Key,
    since: Instant,
    waiting: Option<Instant>,
}

struct Entry {
    key: Key,
    loaded: Loaded,
    at: Instant,
    bytes: usize,
}

/// What [0] has for the document the cursor is on.
pub enum Resolved<'a> {
    /// The document itself.
    Current(&'a Key, &'a Loaded),
    /// An earlier read of the same file or branch, until the new one lands.
    Stale(&'a Key, &'a Loaded),
    Missing,
}

#[derive(Default)]
pub struct Documents {
    /// Rows scrolled past, clamped when drawn.
    pub scroll: usize,
    /// Rows of [0] as last drawn, for paging.
    pub page: usize,
    /// Whether the document on screen is a log that follows its end, and
    /// whether it is: scrolling up pauses it and reaching the end resumes.
    tails: bool,
    following: bool,
    /// What the scroll belongs to: moving to another item starts afresh.
    item: Option<Item>,
    wanted: Option<Wanted>,
    requested: Vec<Key>,
    cache: Vec<Entry>,
    /// The last document that was current, so a file read again keeps its
    /// text on screen meanwhile.
    shown: Option<(Key, Loaded)>,
    /// Rows built from a document at a width, so a frame doesn't rebuild them.
    rows: Option<(Key, usize, Vec<Line<'static>>)>,
}

impl Documents {
    /// Follows the cursor: `hover` is what it's on, if [0] reads it.
    pub fn settle(&mut self, hover: Option<&Hover>, now: Instant) {
        let Some(hover) = hover else {
            *self = Self {
                requested: std::mem::take(&mut self.requested),
                cache: std::mem::take(&mut self.cache),
                scroll: self.scroll,
                page: self.page,
                ..Self::default()
            };
            return;
        };
        let item = hover.item();
        if self.item.as_ref() != Some(&item) {
            self.item = Some(item);
            self.scroll = 0;
            self.following = true;
        }
        let Some(key) = hover.key() else {
            self.wanted = None;
            self.shown = None;
            return;
        };
        self.cache
            .retain(|e| e.loaded.is_ok() || now < e.at + ERROR_TTL);
        if self.wanted.as_ref().map(|w| &w.key) != Some(&key) {
            self.wanted = Some(Wanted {
                key: key.clone(),
                since: now,
                waiting: None,
            });
        }
        if self
            .shown
            .as_ref()
            .is_some_and(|(k, _)| k.slot() != key.slot())
        {
            self.shown = None;
        }
        let hit = self.cache.iter().position(|e| e.key == key);
        if let Some(at) = hit {
            let entry = self.cache.remove(at);
            self.shown = Some((entry.key.clone(), entry.loaded.clone()));
            self.cache.push(entry);
        }
        if let Some(wanted) = &mut self.wanted {
            wanted.waiting = match hit {
                Some(_) => None,
                None => wanted.waiting.or(Some(now)),
            };
        }
    }

    /// The hovered document, once the cursor has rested on it and nothing
    /// has it or is reading it.
    pub fn due(&mut self, now: Instant) -> Option<Key> {
        let wanted = self.wanted.as_ref()?;
        let due = now >= wanted.since + REST
            && !self.cache.iter().any(|e| e.key == wanted.key)
            && !self.requested.contains(&wanted.key);
        let key = due.then(|| wanted.key.clone())?;
        self.requested.push(key.clone());
        Some(key)
    }

    /// A read finished. Whatever it was for is cached, shown or not, unless
    /// it failed, which is shown for a few seconds and then asked again.
    /// Older requests for the same slot were replaced and won't answer.
    pub fn loaded(&mut self, key: &Key, result: Result<Doc, String>) {
        let Some(at) = self.requested.iter().position(|k| k == key) else {
            return;
        };
        let slot = key.slot();
        let mut index = 0;
        self.requested.retain(|k| {
            let drop = index <= at && k.slot() == slot;
            index += 1;
            !drop
        });
        self.cache.retain(|e| e.key != *key);
        let bytes = result.as_ref().map_or(0, Doc::bytes);
        self.cache.push(Entry {
            key: key.clone(),
            loaded: Arc::new(result),
            at: Instant::now(),
            bytes,
        });
        self.rows = None;
        while self.cache.len() > 1
            && (self.cache.len() > CACHE
                || self.cache.iter().map(|e| e.bytes).sum::<usize>() > BUDGET)
        {
            self.cache.remove(0);
        }
    }

    /// What [0] has for the hovered document.
    pub fn resolve(&self) -> Resolved<'_> {
        let (Some(wanted), Some((key, loaded))) = (&self.wanted, &self.shown) else {
            return Resolved::Missing;
        };
        if *key == wanted.key {
            Resolved::Current(key, loaded)
        } else if key.slot() == wanted.key.slot() {
            Resolved::Stale(key, loaded)
        } else {
            Resolved::Missing
        }
    }

    /// The rows last built for a document.
    pub fn cached(&self) -> &[Line<'static>] {
        self.rows.as_ref().map_or(&[], |(_, _, rows)| rows)
    }

    /// Rows for the document under `key` at `width`, built once.
    pub fn rows(
        &mut self,
        key: &Key,
        width: usize,
        build: impl FnOnce() -> Vec<Line<'static>>,
    ) -> &[Line<'static>] {
        if !self
            .rows
            .as_ref()
            .is_some_and(|(k, w, _)| k == key && *w == width)
        {
            self.rows = Some((key.clone(), width, build()));
        }
        self.rows.as_ref().map_or(&[], |(_, _, rows)| rows)
    }

    /// Whether what's shown is for an earlier cursor position and the read
    /// is taking long enough to say so.
    pub fn loading(&self, now: Instant) -> bool {
        self.wanted
            .as_ref()
            .and_then(|w| w.waiting)
            .is_some_and(|since| now >= since + LOADING_AFTER)
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
        if delta < 0 {
            self.following = false;
        }
    }

    pub fn scroll_to_start(&mut self) {
        self.scroll = 0;
        self.following = false;
    }

    /// Past the last row, which drawing clamps to it, and following again.
    pub fn scroll_to_end(&mut self) {
        self.scroll = usize::MAX;
        self.following = true;
    }

    /// The first row shown of `total` rows in `height`, and the scroll kept
    /// at it. A log that follows its end stays on it as rows are added until
    /// scrolled up, and following resumes on reaching the end again.
    pub fn place(&mut self, total: usize, height: usize, tails: bool) -> usize {
        let last = total.saturating_sub(height);
        self.tails = tails;
        self.page = height;
        let top = if tails && self.following {
            last
        } else {
            self.scroll.min(last)
        };
        if tails && top >= last {
            self.following = true;
        }
        self.scroll = top;
        top
    }

    /// Whether the log on screen follows its end.
    pub fn following(&self) -> bool {
        self.tails && self.following
    }

    /// Whether the log on screen was scrolled away from its end.
    pub fn paused(&self) -> bool {
        self.tails && !self.following
    }

    /// Half the rows of [0], at least one.
    pub fn half_page(&self) -> isize {
        isize::try_from(self.page / 2).unwrap_or(isize::MAX).max(1)
    }

    /// Drops everything about an agent that's gone.
    pub fn forget(&mut self, known: impl Fn(&SessionName) -> bool) {
        self.cache.retain(|e| known(e.key.name()));
        self.requested.retain(|k| known(k.name()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str, added: usize) -> DiffKey {
        DiffKey {
            name: "fix".parse().expect("parses"),
            path: path.into(),
            lines: Lines::Counted { added, removed: 0 },
            base: DiffBase::Fork,
            generation: 0,
        }
    }

    fn diff(text: &str) -> Doc {
        Doc::Diff(FileDiff::Text {
            lines: vec![text.into()],
            more: 0,
        })
    }

    fn path_of(docs: &Documents) -> Option<PathBuf> {
        match docs.shown.as_ref().map(|(k, _)| k) {
            Some(Key::Diff(key)) => Some(key.path.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_read_starts_after_the_cursor_rests_and_the_old_document_stays_until_it_lands() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.settle(Some(&Hover::Diff(key("a", 1))), t0);
        assert_eq!(docs.due(t0), None, "not at once");
        let a = docs.due(t0 + REST).expect("after the rest");
        assert_eq!(docs.due(t0 + REST), None, "once");
        docs.loaded(&a, Ok(diff("a")));
        docs.settle(Some(&Hover::Diff(key("a", 1))), t0 + REST);
        assert_eq!(path_of(&docs), Some("a".into()));

        let t1 = t0 + REST * 2;
        docs.settle(Some(&Hover::Diff(key("b", 2))), t1);
        assert_eq!(path_of(&docs), None, "a never stands in for b");
        assert!(!docs.loading(t1 + Duration::from_millis(100)));
        assert!(docs.loading(t1 + LOADING_AFTER));
        let b = docs.due(t1 + REST).expect("b is read");
        docs.loaded(&b, Ok(diff("b")));
        docs.settle(Some(&Hover::Diff(key("b", 2))), t1 + REST);
        assert_eq!(path_of(&docs), Some("b".into()));
        assert!(!docs.loading(t1 + Duration::from_secs(9)));

        docs.settle(Some(&Hover::Diff(key("a", 1))), t1 + REST * 2);
        assert_eq!(
            path_of(&docs),
            Some("a".into()),
            "a cache hit shows at once"
        );
        assert_eq!(
            docs.due(t1 + Duration::from_secs(1)),
            None,
            "and isn't read"
        );
    }

    #[test]
    fn scroll_resets_for_another_file_but_not_when_the_same_file_changes() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.settle(Some(&Hover::Diff(key("a", 1))), t0);
        docs.scroll_by(7);
        docs.settle(Some(&Hover::Diff(key("a", 5))), t0);
        assert_eq!(docs.scroll, 7, "same file, new numstat line");
        docs.settle(Some(&Hover::Diff(key("b", 1))), t0);
        assert_eq!(docs.scroll, 0);
        docs.scroll_by(-3);
        assert_eq!(docs.scroll, 0);
    }

    #[test]
    fn the_cache_keeps_the_last_few_and_an_answer_nobody_asked_for_is_dropped() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.loaded(&Key::Diff(key("x", 1)), Ok(diff("x")));
        docs.settle(Some(&Hover::Diff(key("x", 1))), t0);
        assert!(docs.shown.is_none(), "never requested");
        for i in 0..CACHE + 5 {
            let k = key(&format!("f{i}"), i);
            docs.settle(Some(&Hover::Diff(k.clone())), t0);
            let due = docs.due(t0 + REST).expect("due");
            docs.loaded(&due, Ok(diff("d")));
        }
        assert_eq!(docs.cache.len(), CACHE);
        assert!(docs.cache.iter().all(|e| {
            !matches!(&e.key, Key::Diff(d) if d.path.as_path() == std::path::Path::new("f0"))
        }));
    }

    #[test]
    fn another_files_text_never_stands_in_and_the_same_files_older_read_does() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.settle(Some(&Hover::Diff(key("a", 1))), t0);
        let a = docs.due(t0 + REST).expect("due");
        docs.loaded(&a, Ok(diff("a")));
        docs.settle(Some(&Hover::Diff(key("a", 1))), t0 + REST);
        assert!(matches!(docs.resolve(), Resolved::Current(..)));

        docs.settle(Some(&Hover::Diff(key("a", 2))), t0 + REST * 2);
        assert!(
            matches!(docs.resolve(), Resolved::Stale(..)),
            "the same file read again keeps its text meanwhile"
        );
        docs.settle(Some(&Hover::Diff(key("b", 1))), t0 + REST * 3);
        assert!(
            matches!(docs.resolve(), Resolved::Missing),
            "b never shows a's hunks, however slow its read is"
        );
        docs.settle(
            Some(&Hover::Branch(BranchKey {
                name: "fix".parse().expect("parses"),
                ahead: 1,
                behind: 0,
                dirty: 0,
                generation: 0,
            })),
            t0 + REST * 3,
        );
        assert!(matches!(docs.resolve(), Resolved::Missing));
    }

    #[test]
    fn a_request_replaced_before_it_started_is_asked_again_and_errors_are_asked_again_later() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.settle(Some(&Hover::Diff(key("a", 1))), t0);
        let first = docs.due(t0 + REST).expect("due");
        docs.settle(Some(&Hover::Diff(key("a", 2))), t0 + REST);
        let second = docs
            .due(t0 + REST * 2)
            .expect("the newer one replaces the older in the queue");
        assert_ne!(first, second);
        docs.loaded(&second, Ok(diff("two")));
        assert!(
            docs.requested.is_empty(),
            "the replaced request left nothing behind"
        );
        docs.settle(Some(&Hover::Diff(key("a", 1))), t0 + REST * 3);
        assert!(
            docs.due(t0 + REST * 5).is_some(),
            "going back to the replaced one asks for it again"
        );

        docs.settle(Some(&Hover::Diff(key("c", 1))), t0);
        let c = docs.due(t0 + REST).expect("due");
        docs.loaded(&c, Err("git failed".into()));
        docs.settle(Some(&Hover::Diff(key("c", 1))), t0 + REST);
        assert!(
            matches!(docs.resolve(), Resolved::Current(..)),
            "the error shows"
        );
        assert_eq!(docs.due(t0 + REST * 2), None);
        docs.settle(Some(&Hover::Diff(key("c", 1))), t0 + ERROR_TTL + REST);
        assert!(
            docs.due(t0 + ERROR_TTL + REST * 2).is_some(),
            "and is asked again"
        );
    }

    #[test]
    fn the_cache_has_a_byte_budget() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        let big = |n: usize| {
            Doc::Diff(FileDiff::Text {
                lines: vec!["x".repeat(BUDGET / 3)],
                more: n,
            })
        };
        for i in 0..5 {
            docs.settle(Some(&Hover::Diff(key(&format!("f{i}"), i))), t0);
            let due = docs.due(t0 + REST).expect("due");
            docs.loaded(&due, Ok(big(i)));
        }
        assert!(docs.cache.len() <= 3, "{}", docs.cache.len());
    }

    fn log() -> Hover {
        Hover::Subagent("fix".parse().expect("parses"), AgentId::new("s1"))
    }

    #[test]
    fn a_log_follows_its_end_pauses_when_scrolled_up_and_resumes_at_the_end() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.settle(Some(&log()), t0);
        assert_eq!(docs.place(30, 10, true), 20, "opens at the tail");
        assert!(docs.following());
        assert_eq!(docs.place(35, 10, true), 25, "and follows as rows arrive");

        docs.scroll_by(-3);
        assert_eq!(docs.place(35, 10, true), 22);
        assert!(docs.paused());
        assert_eq!(
            docs.place(40, 10, true),
            22,
            "rows arriving don't move a paused log"
        );

        docs.scroll_by(2);
        assert_eq!(docs.place(40, 10, true), 24);
        assert!(docs.paused());
        docs.scroll_by(99);
        assert_eq!(docs.place(40, 10, true), 30);
        assert!(docs.following(), "scrolling back to the end resumes");

        docs.scroll_by(-1);
        docs.scroll_to_end();
        assert_eq!(docs.place(44, 10, true), 34, "G resumes at once");
        docs.scroll_to_start();
        assert_eq!(docs.place(44, 10, true), 0);
        assert!(docs.paused());

        docs.settle(Some(&Hover::Turn("fix".parse().expect("parses"))), t0);
        assert_eq!(docs.place(44, 10, false), 0, "a document opens at the top");
        assert!(
            !docs.following() && !docs.paused(),
            "and has nothing to follow"
        );
        docs.settle(Some(&log()), t0);
        assert_eq!(
            docs.place(44, 10, true),
            34,
            "returning to a log starts at its tail"
        );
    }
}
