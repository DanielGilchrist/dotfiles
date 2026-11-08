//! What the main view [0] reads when it isn't the terminal: documents
//! loaded off the UI thread, a cache of the last few, and the scroll
//! position of the one on screen.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::names::SessionName;
use agent_core::ports::{FileDiff, Lines};

/// The cursor must rest on a file this long before its diff is asked for, so
/// holding `j` doesn't start a read per file.
pub const REST: Duration = Duration::from_millis(60);
/// A document that takes longer than this shows `loading`.
pub const LOADING_AFTER: Duration = Duration::from_millis(150);
/// Documents kept, the least recently shown going first.
const CACHE: usize = 32;

/// A changed file's diff: whose it is, which file, and the numstat line it
/// was read at, so a file that changed again is a different document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffKey {
    pub name: SessionName,
    pub path: PathBuf,
    pub lines: Lines,
}

pub type Loaded = Arc<Result<FileDiff, String>>;

#[derive(Default)]
pub struct Documents {
    /// Rows scrolled past, clamped when drawn.
    pub scroll: usize,
    /// Rows of [0] as last drawn, for paging.
    pub page: usize,
    /// What the scroll belongs to: moving to another file starts at the top.
    item: Option<(SessionName, PathBuf)>,
    /// The diff the cursor is on and since when.
    hover: Option<(DiffKey, Instant)>,
    requested: Vec<DiffKey>,
    cache: Vec<(DiffKey, Loaded)>,
    /// What [0] shows: the hovered diff once it's loaded, else the last one
    /// shown, so moving the cursor never blanks the screen.
    shown: Option<(DiffKey, Loaded)>,
    /// Since when [0] has shown something other than what's hovered.
    waiting: Option<Instant>,
}

impl Documents {
    /// Follows the cursor: `hover` is the diff it's on, if any.
    pub fn settle(&mut self, hover: Option<DiffKey>, now: Instant) {
        let Some(key) = hover else {
            self.hover = None;
            self.item = None;
            self.shown = None;
            self.waiting = None;
            return;
        };
        if self.hover.as_ref().map(|(k, _)| k) != Some(&key) {
            self.hover = Some((key.clone(), now));
        }
        let item = (key.name.clone(), key.path.clone());
        if self.item.as_ref() != Some(&item) {
            self.item = Some(item);
            self.scroll = 0;
        }
        match self.cache.iter().position(|(k, _)| *k == key) {
            Some(at) => {
                let hit = self.cache.remove(at);
                self.shown = Some(hit.clone());
                self.cache.push(hit);
                self.waiting = None;
            }
            None => {
                self.waiting.get_or_insert(now);
            }
        }
    }

    /// The hovered diff, once the cursor has rested on it and nothing has
    /// it or is already reading it.
    pub fn due(&mut self, now: Instant) -> Option<DiffKey> {
        let (key, since) = self.hover.as_ref()?;
        let waiting = now >= *since + REST
            && !self.cache.iter().any(|(k, _)| k == key)
            && !self.requested.contains(key);
        let key = waiting.then(|| key.clone())?;
        self.requested.push(key.clone());
        Some(key)
    }

    /// A read finished. Whatever it was for is cached, shown or not.
    pub fn loaded(&mut self, key: &DiffKey, result: Result<FileDiff, String>) {
        let Some(at) = self.requested.iter().position(|k| k == key) else {
            return;
        };
        self.requested.remove(at);
        self.cache.retain(|(k, _)| k != key);
        self.cache.push((key.clone(), Arc::new(result)));
        let excess = self.cache.len().saturating_sub(CACHE);
        self.cache.drain(..excess);
    }

    /// What [0] shows, and the file it's for.
    pub fn shown(&self) -> Option<&(DiffKey, Loaded)> {
        self.shown.as_ref()
    }

    /// Whether what's shown is for an earlier cursor position and the read
    /// is taking long enough to say so.
    pub fn loading(&self, now: Instant) -> bool {
        self.waiting
            .is_some_and(|since| now >= since + LOADING_AFTER)
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
    }

    pub fn scroll_to_start(&mut self) {
        self.scroll = 0;
    }

    /// Past the last row, which drawing clamps to it.
    pub fn scroll_to_end(&mut self) {
        self.scroll = usize::MAX;
    }

    /// Half the rows of [0], at least one.
    pub fn half_page(&self) -> isize {
        isize::try_from(self.page / 2).unwrap_or(isize::MAX).max(1)
    }

    /// Drops everything about an agent that's gone.
    pub fn forget(&mut self, known: impl Fn(&SessionName) -> bool) {
        self.cache.retain(|(k, _)| known(&k.name));
        self.requested.retain(|k| known(&k.name));
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
        }
    }

    fn diff(text: &str) -> FileDiff {
        FileDiff::Text {
            lines: vec![text.into()],
            more: 0,
        }
    }

    #[test]
    fn a_read_starts_after_the_cursor_rests_and_the_old_document_stays_until_it_lands() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.settle(Some(key("a", 1)), t0);
        assert_eq!(docs.due(t0), None, "not at once");
        let a = docs.due(t0 + REST).expect("after the rest");
        assert_eq!(docs.due(t0 + REST), None, "once");
        docs.loaded(&a, Ok(diff("a")));
        docs.settle(Some(key("a", 1)), t0 + REST);
        assert_eq!(docs.shown().map(|(k, _)| k.path.clone()), Some("a".into()));

        let t1 = t0 + REST * 2;
        docs.settle(Some(key("b", 2)), t1);
        assert_eq!(
            docs.shown().map(|(k, _)| k.path.clone()),
            Some("a".into()),
            "a still shows while b is read"
        );
        assert!(!docs.loading(t1 + Duration::from_millis(100)));
        assert!(docs.loading(t1 + LOADING_AFTER));
        let b = docs.due(t1 + REST).expect("b is read");
        docs.loaded(&b, Ok(diff("b")));
        docs.settle(Some(key("b", 2)), t1 + REST);
        assert_eq!(docs.shown().map(|(k, _)| k.path.clone()), Some("b".into()));
        assert!(!docs.loading(t1 + Duration::from_secs(9)));

        docs.settle(Some(key("a", 1)), t1 + REST * 2);
        assert_eq!(
            docs.shown().map(|(k, _)| k.path.clone()),
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
        docs.settle(Some(key("a", 1)), t0);
        docs.scroll_by(7);
        docs.settle(Some(key("a", 5)), t0);
        assert_eq!(docs.scroll, 7, "same file, new numstat line");
        docs.settle(Some(key("b", 1)), t0);
        assert_eq!(docs.scroll, 0);
        docs.scroll_by(-3);
        assert_eq!(docs.scroll, 0);
    }

    #[test]
    fn the_cache_keeps_the_last_few_and_an_answer_nobody_asked_for_is_dropped() {
        let mut docs = Documents::default();
        let t0 = Instant::now();
        docs.loaded(&key("x", 1), Ok(diff("x")));
        docs.settle(Some(key("x", 1)), t0);
        assert!(docs.shown().is_none(), "never requested");
        for i in 0..CACHE + 5 {
            let k = key(&format!("f{i}"), i);
            docs.settle(Some(k.clone()), t0);
            docs.hover = Some((k.clone(), t0));
            let due = docs.due(t0 + REST).expect("due");
            docs.loaded(&due, Ok(diff("d")));
        }
        assert_eq!(docs.cache.len(), CACHE);
        assert!(
            docs.cache
                .iter()
                .all(|(k, _)| k.path.as_path() != std::path::Path::new("f0"))
        );
    }
}
