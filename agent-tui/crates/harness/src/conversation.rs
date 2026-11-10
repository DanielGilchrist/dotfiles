//! Which transcript is a conversation's own. Claude Code moves a session to
//! the background by forking it: the background worker has a new session id,
//! and the original transcript ends in `continued-in` records naming the
//! fork. A transcript the hooks or the directory listing point at can
//! therefore be one the conversation has left.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use serde::Deserialize;

use crate::{TAIL_BYTES, has_user_prompt, read_tail};

/// How much of the end of a transcript is searched for `continued-in`
/// records. They are written when the session moves, after which nothing
/// writes to that transcript.
const CONTINUATION_BYTES: u64 = 256 * 1024;

const CONTINUED_IN: &str = "continued-in";

/// A Claude Code session id, which names `<id>.jsonl`. It is read from
/// files and used in paths and arguments, so it is only ever plain
/// characters and can't be taken for a flag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct SessionId(String);

impl SessionId {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let plain = raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        (plain && !raw.is_empty() && !raw.starts_with('-')).then(|| Self(raw.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn transcript_in(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.jsonl", self.0))
    }

    fn has_messages_in(&self, dir: &Path) -> io::Result<bool> {
        match has_user_prompt(&self.transcript_in(dir)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            other => other,
        }
    }
}

impl TryFrom<String> for SessionId {
    type Error = &'static str;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw).ok_or("not a session id")
    }
}

#[derive(Deserialize)]
struct Continuation {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId")]
    from: SessionId,
    #[serde(rename = "continuedInSessionId")]
    to: SessionId,
}

/// The session ids one conversation has run under, oldest first, and which
/// of them holds the messages. A fork made to run in the background may have
/// no messages of its own, and `--resume` refuses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conversation {
    ids: Vec<SessionId>,
    /// An index into `ids`, which is never empty.
    holder: usize,
}

impl Conversation {
    fn starting_at(id: SessionId) -> Self {
        Self {
            ids: vec![id],
            holder: 0,
        }
    }

    /// The conversation `start` belongs to, followed through the
    /// `continued-in` records in `dir` to its newest session.
    pub(crate) fn of(dir: &Path, start: SessionId) -> io::Result<Self> {
        let mut found = Self::starting_at(start.clone());
        let mut from = start;
        while let Some(to) = continuation(dir, &from)? {
            if found.ids.contains(&to) {
                break;
            }
            found.then(to.clone(), to.has_messages_in(dir)?);
            from = to;
        }
        Ok(found)
    }

    fn then(&mut self, id: SessionId, has_messages: bool) {
        self.ids.push(id);
        if has_messages {
            self.holder = self.ids.len() - 1;
        }
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = &SessionId> {
        self.ids.iter()
    }

    /// The session to `--resume`: the newest that holds messages, or the one
    /// the conversation was looked up from when none does.
    pub(crate) fn resumable(&self) -> &SessionId {
        &self.ids[self.holder]
    }

    /// The transcript to read for what the conversation is doing.
    pub(crate) fn transcript_in(&self, dir: &Path) -> PathBuf {
        self.resumable().transcript_in(dir)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

struct Remembered {
    conversation: Conversation,
    stamps: Vec<Option<FileStamp>>,
}

impl Remembered {
    fn holds(&self, dir: &Path) -> bool {
        self.conversation
            .ids()
            .zip(&self.stamps)
            .all(|(id, stamp)| FileStamp::of(&id.transcript_in(dir)) == *stamp)
    }
}

/// Conversations by the transcript they were looked up from. A lookup reads
/// the end of every transcript in a chain, and it is asked for every few
/// seconds per agent, so one is reused until a file in its chain changes.
#[derive(Default)]
pub(crate) struct Chains(Mutex<HashMap<PathBuf, Remembered>>);

impl Chains {
    pub(crate) fn of(&self, dir: &Path, start: SessionId) -> io::Result<Conversation> {
        let key = start.transcript_in(dir);
        let mut chains = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(remembered) = chains.get(&key).filter(|r| r.holds(dir)) {
            return Ok(remembered.conversation.clone());
        }
        let conversation = Conversation::of(dir, start)?;
        let stamps = conversation
            .ids()
            .map(|id| FileStamp::of(&id.transcript_in(dir)))
            .collect();
        chains.insert(
            key,
            Remembered {
                conversation: conversation.clone(),
                stamps,
            },
        );
        Ok(conversation)
    }
}

/// The session `id` was last continued in, by its own transcript.
fn continuation(dir: &Path, id: &SessionId) -> io::Result<Option<SessionId>> {
    let mut file = match std::fs::File::open(id.transcript_in(dir)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let tail = read_tail(&mut file, len, CONTINUATION_BYTES, TAIL_BYTES)?;
    Ok(tail
        .text()
        .lines()
        .filter(|line| line.contains(CONTINUED_IN))
        .filter_map(|line| serde_json::from_str::<Continuation>(line).ok())
        .rfind(|record| record.kind == CONTINUED_IN && record.from == *id)
        .map(|record| record.to))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::fixtures::{continued, user_line, write_at};

    fn id(raw: &str) -> SessionId {
        SessionId::parse(raw).expect("a session id")
    }

    fn dir_of(files: &[(&str, Vec<String>)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");
        for (name, lines) in files {
            write_at(dir.path(), name, lines, 0);
        }
        dir
    }

    fn user() -> String {
        user_line()
    }

    fn bare() -> String {
        r#"{"type":"summary"}"#.to_string()
    }

    fn ids(conversation: &Conversation) -> Vec<&str> {
        conversation.ids().map(SessionId::as_str).collect()
    }

    #[test]
    fn session_ids_are_plain_and_never_flags_or_paths() {
        for refused in ["", "-x", "../x", "a/b", "a b", "a.jsonl", "a\n"] {
            assert!(SessionId::parse(refused).is_none(), "{refused:?}");
        }
        assert!(SessionId::parse("470bb73a-cfe7_4225").is_some());
    }

    #[test]
    fn a_transcript_nothing_continued_is_its_own_conversation() {
        let dir = dir_of(&[("a", vec![user()])]);
        let found = Conversation::of(dir.path(), id("a")).expect("reads");
        assert_eq!(ids(&found), ["a"]);
        assert_eq!(found.resumable().as_str(), "a");
    }

    #[test]
    fn a_superseded_transcript_leads_to_the_fork_that_holds_the_messages() {
        let dir = dir_of(&[
            ("old", vec![user(), continued("old", "new")]),
            ("new", vec![user(), user()]),
        ]);
        let found = Conversation::of(dir.path(), id("old")).expect("reads");
        assert_eq!(ids(&found), ["old", "new"]);
        assert_eq!(found.resumable().as_str(), "new");
        assert_eq!(
            found.transcript_in(dir.path()),
            dir.path().join("new.jsonl")
        );
    }

    #[test]
    fn a_fork_with_no_messages_is_followed_but_not_resumed() {
        let dir = dir_of(&[
            ("old", vec![user(), continued("old", "stub")]),
            ("stub", vec![bare()]),
        ]);
        let found = Conversation::of(dir.path(), id("old")).expect("reads");
        assert_eq!(ids(&found), ["old", "stub"]);
        assert_eq!(found.resumable().as_str(), "old");
    }

    #[test]
    fn a_fork_with_no_transcript_at_all_is_still_part_of_the_conversation() {
        let dir = dir_of(&[("old", vec![user(), continued("old", "missing")])]);
        let found = Conversation::of(dir.path(), id("old")).expect("reads");
        assert_eq!(ids(&found), ["old", "missing"]);
        assert_eq!(found.resumable().as_str(), "old");
    }

    #[test]
    fn a_chain_of_two_forks_ends_at_the_last_that_holds_messages() {
        let dir = dir_of(&[
            ("a", vec![user(), continued("a", "b")]),
            ("b", vec![user(), continued("b", "c")]),
            ("c", vec![bare()]),
        ]);
        let found = Conversation::of(dir.path(), id("a")).expect("reads");
        assert_eq!(ids(&found), ["a", "b", "c"]);
        assert_eq!(found.resumable().as_str(), "b");

        let full = dir_of(&[
            ("a", vec![user(), continued("a", "b")]),
            ("b", vec![user(), continued("b", "c")]),
            ("c", vec![user()]),
        ]);
        let found = Conversation::of(full.path(), id("a")).expect("reads");
        assert_eq!(found.resumable().as_str(), "c");
    }

    #[test]
    fn a_session_continued_twice_from_the_same_transcript_follows_the_last_record() {
        let dir = dir_of(&[
            ("a", vec![user(), continued("a", "b"), continued("a", "c")]),
            ("b", vec![user()]),
            ("c", vec![user()]),
        ]);
        let found = Conversation::of(dir.path(), id("a")).expect("reads");
        assert_eq!(ids(&found), ["a", "c"]);
    }

    #[test]
    fn records_about_another_session_or_that_are_not_continuations_are_ignored() {
        let lie = r#"{"type":"user","message":{"content":"continued-in"}}"#.to_string();
        let dir = dir_of(&[(
            "a",
            vec![user(), lie, continued("someone-else", "b"), user()],
        )]);
        let found = Conversation::of(dir.path(), id("a")).expect("reads");
        assert_eq!(ids(&found), ["a"]);
    }

    #[test]
    fn a_loop_of_continuations_ends() {
        let dir = dir_of(&[
            ("a", vec![user(), continued("a", "b")]),
            ("b", vec![user(), continued("b", "a")]),
        ]);
        let found = Conversation::of(dir.path(), id("a")).expect("reads");
        assert_eq!(ids(&found), ["a", "b"]);
    }

    #[test]
    fn a_continuation_naming_an_unsafe_id_is_dropped() {
        let dir = dir_of(&[("a", vec![user(), continued("a", "../escape")])]);
        let found = Conversation::of(dir.path(), id("a")).expect("reads");
        assert_eq!(ids(&found), ["a"]);
    }
}
