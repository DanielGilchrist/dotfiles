//! Background sessions. Claude Code keeps a session running when its
//! terminal goes, and `--resume` refuses one that is alive, so it has to be
//! attached to or stopped instead, by the job id its registry records.

use std::io;
use std::path::{Path, PathBuf};

use agent_core::activity::Pid;
use agent_core::ports::HarnessError;
use agent_core::process::quiet;
use serde::{Deserialize, Deserializer};

use crate::PS_TIMEOUT;
use crate::conversation::{Conversation, SessionId};

/// The short id `claude attach` and `claude stop` take. They refuse the full
/// session id, and the charset rule is a session id's, which also keeps a
/// flag out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobId(SessionId);

impl JobId {
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, PartialEq, Eq, Default)]
pub(crate) enum JobField {
    #[default]
    Missing,
    /// Present but not an id `claude` would take, as written.
    Malformed(String),
    Valid(JobId),
}

impl<'de> Deserialize<'de> for JobField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let written = serde_json::Value::deserialize(deserializer)?;
        Ok(match written {
            serde_json::Value::Null => Self::Missing,
            serde_json::Value::String(raw) => match SessionId::parse(&raw) {
                Some(id) => Self::Valid(JobId(id)),
                None => Self::Malformed(raw),
            },
            other => Self::Malformed(other.to_string()),
        })
    }
}

/// When a process started, as `ps` prints it and Claude Code's registry
/// records it. A pid alone can be another process's after a reboot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub(crate) struct ProcStart(String);

impl From<String> for ProcStart {
    fn from(raw: String) -> Self {
        Self(raw.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

impl std::fmt::Display for ProcStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl ProcStart {
    /// When `pid` started, if the process table has it. `ps` also answers
    /// for processes the caller may not signal.
    pub(crate) fn of(pid: Pid) -> Option<Self> {
        let mut cmd = quiet("ps");
        cmd.args(["-o", "lstart=", "-p", &pid.get().to_string()]);
        let out = agent_core::process::output_within(&mut cmd, PS_TIMEOUT).ok()?;
        let started = Self::from(String::from_utf8_lossy(&out.stdout).into_owned());
        (out.status.success() && !started.0.is_empty()).then_some(started)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Survivor {
    pub(crate) job: JobId,
    pub(crate) pid: Pid,
    pub(crate) started: ProcStart,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unaddressed {
    Missing,
    Malformed(String),
}

impl std::fmt::Display for Unaddressed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("no job id"),
            Self::Malformed(raw) => write!(f, "the job id {raw:?}, which `claude` would not take"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Attachable(Survivor),
    Unaddressable { pid: Pid, why: Unaddressed },
}

impl Survivor {
    fn confirm(registered: Registered) -> Option<Found> {
        if ProcStart::of(registered.pid).as_ref() != Some(&registered.started) {
            return None;
        }
        let pid = registered.pid;
        Some(match registered.job {
            JobField::Valid(job) => Found::Attachable(Self {
                job,
                pid,
                started: registered.started,
            }),
            JobField::Missing => Found::Unaddressable {
                pid,
                why: Unaddressed::Missing,
            },
            JobField::Malformed(raw) => Found::Unaddressable {
                pid,
                why: Unaddressed::Malformed(raw),
            },
        })
    }
}

#[derive(Deserialize)]
enum Kind {
    #[serde(rename = "bg")]
    Background,
    #[serde(other)]
    Other,
}

/// A spare is a worker kept warm with no conversation.
#[derive(Deserialize, Default)]
#[serde(from = "bool")]
enum Role {
    #[default]
    Worker,
    Spare,
}

impl From<bool> for Role {
    fn from(spare: bool) -> Self {
        if spare { Self::Spare } else { Self::Worker }
    }
}

#[derive(Deserialize)]
struct Registered {
    pid: Pid,
    #[serde(rename = "sessionId")]
    session: SessionId,
    kind: Kind,
    #[serde(default)]
    spare: Role,
    #[serde(rename = "procStart")]
    started: ProcStart,
    cwd: PathBuf,
    #[serde(rename = "jobId", default)]
    job: JobField,
}

#[derive(Deserialize)]
struct Whose {
    #[serde(rename = "sessionId")]
    session: SessionId,
}

/// Whether a worker's directory is the agent's. The session ids of a chain
/// come from this directory's own transcripts, so a match is already
/// specific to it. The worker may still have been started elsewhere or be
/// reached through a link, so the directory only breaks ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Here,
    Elsewhere,
}

impl Registered {
    fn works_on(&self, conversation: &Conversation) -> bool {
        matches!((&self.kind, &self.spare), (Kind::Background, Role::Worker))
            && conversation.ids().any(|id| *id == self.session)
    }

    fn place(&self, workdir: &Path) -> Place {
        if self.cwd.canonicalize().is_ok_and(|cwd| cwd == workdir) {
            Place::Here
        } else {
            Place::Elsewhere
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Search {
    pub(crate) found: Option<Found>,
    /// Entries of the conversation's sessions that couldn't be read, which
    /// may be its worker.
    pub(crate) unreadable: Vec<PathBuf>,
}

/// The background session running `conversation`, preferring one with a job
/// id and then one in `workdir`. An entry whose process is gone, a spare, or
/// another conversation's doesn't count. A file that vanishes or isn't text
/// between listing and reading is skipped like any other junk.
pub(crate) fn find(
    sessions: &Path,
    conversation: &Conversation,
    workdir: &Path,
) -> Result<Search, HarnessError> {
    let read = |source| HarnessError {
        context: "reading Claude's session registry".into(),
        source,
    };
    let entries = match std::fs::read_dir(sessions) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Search {
                found: None,
                unreadable: Vec::new(),
            });
        }
        Err(e) => return Err(read(e)),
    };
    let mut candidates = Vec::new();
    let mut unreadable = Vec::new();
    for entry in entries {
        let path = entry.map_err(read)?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<Registered>(&text) {
            Ok(registered) if registered.works_on(conversation) => {
                let place = registered.place(workdir);
                candidates.extend(Survivor::confirm(registered).map(|found| (place, found)));
            }
            Ok(_) => {}
            Err(_) => {
                let ours = serde_json::from_str::<Whose>(&text)
                    .is_ok_and(|whose| conversation.ids().any(|id| *id == whose.session));
                if ours {
                    unreadable.push(path);
                }
            }
        }
    }
    let found = candidates
        .into_iter()
        .min_by_key(|(place, found)| (matches!(found, Found::Unaddressable { .. }), *place))
        .map(|(_, found)| found);
    Ok(Search { found, unreadable })
}
