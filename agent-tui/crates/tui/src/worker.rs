use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use agent_core::actions::CommandLine;
use std::time::SystemTime;

use agent_core::activity::{Activity, AgentId, TranscriptRef};
use agent_core::config::Sources;
use agent_core::discover;
use agent_core::model::{Removable, Running, Snapshot, Startable};
use agent_core::names::{AgentName, Prompt, SessionName};
use agent_core::ops::{self, Approved, Backend, Launch, RemovalPlan, Removed};
use agent_core::paths::RepoRoot;
use agent_core::ports::{
    ActivityDetail, ClientId, FileDiff, HarnessId, HostError, Log, Place, Window,
};
use crossterm::event::{KeyEvent, MouseEvent};

use crate::document::{Doc, Key, assemble};
use crate::model::{Company, Found, Inspected, Launchable, Look, Need, RepoChoice, Ticket};

/// How much of a failed action's stderr is read for its last line.
const STDERR_TAIL: u64 = 64 * 1024;

/// Threads reading git facts, so a slow repo holds up only its own agents.
const INSPECTORS: usize = 2;

/// Threads reading subagent transcripts.
const SUBAGENT_READERS: usize = 1;

/// Threads reading diffs and branch logs.
const DOCUMENT_READERS: usize = 2;

/// How often the refresh thread checks what agents reported through hooks.
const ACTIVITY_POLL: std::time::Duration = std::time::Duration::from_millis(500);

pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    Snapshot(Result<(Snapshot, Vec<String>), String>),
    Launched {
        ticket: Ticket,
        result: Result<Launch, String>,
    },
    RemovalPlanned {
        ticket: Ticket,
        result: Result<RemovalPlan, String>,
    },
    Done(Result<Outcome, String>),
    /// A message was typed into `to`, or wasn't.
    Sent {
        to: SessionName,
        result: Result<(), SendError>,
    },
    /// The windows besides this TUI's showing each session that has any,
    /// for the look `ticket` asked for, or why that couldn't be told.
    Elsewhere {
        ticket: Ticket,
        found: Sightings,
    },
    /// The harness's model named the prompt of `generation` (or couldn't).
    NameSuggested {
        generation: u64,
        name: Option<AgentName>,
    },
    /// Other windows showing `name` were closed, or couldn't be.
    ClosedElsewhere {
        name: SessionName,
        closed: Result<usize, String>,
    },
    /// What is at `path`.
    DirChecked {
        path: PathBuf,
        found: Found,
    },
    /// The terminal gained or lost focus.
    Focus(crate::model::TerminalFocus),
    /// What every agent's harness last reported, when it changed.
    Activities(HashMap<SessionName, Activity>),
    /// The repos under `[repos] dirs` and what couldn't be read there, when
    /// either changed.
    Catalogue {
        repos: Vec<RepoRoot>,
        warnings: Vec<String>,
    },
    /// Git facts about an agent's worktree.
    Inspected {
        name: SessionName,
        read: Result<Inspected, String>,
    },
    /// A document, read for `key`.
    Document {
        key: Key,
        result: Result<Doc, String>,
    },
    /// What a subagent was told and has done, or `None` when its harness
    /// has no record of it yet.
    Subagent {
        name: SessionName,
        id: AgentId,
        detail: Result<Option<Log>, String>,
    },
    /// An agent's current activity, or `None` when its harness has no record of
    /// one.
    Activity {
        name: SessionName,
        detail: Result<Option<ActivityDetail>, String>,
    },
}

pub enum Outcome {
    Killed(SessionName),
    Removed {
        name: SessionName,
        warnings: Vec<String>,
    },
    Pinned(SessionName),
    Unpinned(SessionName),
    /// A repo was asked into the list.
    Listed(ops::Listed),
    /// A configured action's process started.
    Started(String),
    /// It exited successfully.
    Finished(String),
}

/// Why a message wasn't typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The store says the agent isn't at rest: try again when it is.
    Busy,
    /// The store couldn't be read, so nothing was typed: try again.
    Unreadable(String),
    Failed(String),
}

pub enum Job {
    /// Type a message into an agent and submit it, if the store still says
    /// it's at rest.
    Reply {
        to: SessionName,
        prompt: Prompt,
        /// The agent must have reported after this: its session was live
        /// by then, so the record isn't from an earlier run.
        not_before: Option<SystemTime>,
    },
    Start {
        at: Startable,
        harness: HarnessId,
        ticket: Ticket,
    },
    /// What the new-agent form decided, carried out as decided.
    Launch {
        launch: Launchable,
        harness: HarnessId,
        ticket: Ticket,
    },
    Kill(Running),
    /// `own` is the session the TUI runs in, never offered for killing.
    PlanRemoval {
        target: Removable,
        ticket: Ticket,
        own: Option<SessionName>,
    },
    /// `own` is the session the TUI runs in, which removal must refuse.
    Remove {
        approved: Approved,
        own: Option<SessionName>,
    },
    Pin(SessionName),
    Unpin(SessionName),
    /// List the repo holding this directory.
    ListRepo(PathBuf),
    /// Remember the selection for the next start.
    Select(SessionName),
    /// Look at the filesystem for a typed repo path.
    CheckDir(PathBuf),
    /// Close the other windows showing `name`, which moves it here.
    CloseElsewhere {
        name: SessionName,
        own: Option<ClientId>,
    },
    /// The user saw an agent's finished activity.
    Acknowledge(SessionName),
    /// Start a configured action's process; a failure is reported when it exits.
    Run {
        label: String,
        launch: CommandLine,
    },
}

impl Job {
    /// Whether the snapshot should be refreshed after this job.
    fn mutates(&self) -> bool {
        match self {
            Self::Start { .. }
            | Self::Launch { .. }
            | Self::Kill(_)
            | Self::Remove { .. }
            | Self::Pin(_)
            | Self::Unpin(_)
            | Self::ListRepo(_) => true,
            Self::PlanRemoval { .. }
            | Self::Select(_)
            | Self::CheckDir(_)
            | Self::CloseElsewhere { .. }
            | Self::Acknowledge(_)
            | Self::Reply { .. }
            | Self::Run { .. } => false,
        }
    }
}

/// The error and its sources, outermost first.
pub fn describe(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        let text = s.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = s.source();
    }
    out
}

pub struct Workers {
    pub jobs: Sender<Job>,
    inspector: Arc<Inspector>,
    subagents: Arc<SubagentReader>,
    activities: Arc<ActivityReader>,
    documents: Arc<DocumentReader>,
    namer: Arc<Namer>,
    lookouts: Lookouts,
    pub refresh: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Workers {
    /// Asks for git facts about an agent's worktree.
    /// `events` is how many the agent has reported, which the answer carries.
    pub fn inspect(&self, name: SessionName, worktree: PathBuf, events: Option<u64>) {
        self.inspector.request(name, (worktree, events));
    }

    /// Asks for a document about the agent's `worktree`.
    /// `reuse` holds diffs already read, for a folder's to take.
    pub fn read_document(&self, key: Key, worktree: PathBuf, reuse: Vec<(PathBuf, FileDiff)>) {
        let at = key.slot().1;
        self.documents
            .request((key.name().clone(), at), (worktree, key, reuse));
    }

    /// Asks for what subagent `id` of an agent working in `workdir` has done.
    pub fn read_subagent(&self, name: SessionName, id: AgentId, workdir: PathBuf) {
        self.subagents.request((name, id), workdir);
    }

    /// Asks for what the agent working in `workdir` has done this activity, from
    /// the transcript its hooks named.
    pub fn read_activity(&self, name: SessionName, workdir: PathBuf, at: Option<TranscriptRef>) {
        self.activities.request(name, (workdir, at));
    }

    /// Asks which windows besides this TUI's show any session, on threads of
    /// their own so no slow job delays the answer. A decision's ask has a thread
    /// to itself, so a refresh scan still running can't make it late. Only
    /// the latest ask of each kind is answered.
    pub fn look_elsewhere(&self, look: Look) {
        self.lookouts.request(look);
    }

    /// Asks the harness's model to name `prompt`. A request still waiting
    /// when a newer one comes is dropped, so only the latest is answered.
    pub fn suggest_name(&self, prompt: Prompt, harness: HarnessId, generation: u64) {
        self.namer.request((), (prompt, harness, generation));
    }
}

impl Drop for Workers {
    /// Stops taking jobs and waits for the one in flight, however the TUI
    /// exits, so quitting can't interrupt a removal halfway.
    fn drop(&mut self) {
        let (closed, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, closed));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Requests keyed by `K`, served by several threads, one request per key at
/// a time. A request for a key already being served waits its turn, and one
/// waiting is replaced by a newer one, so the answer reflects the latest.
struct Queue<K, V> {
    queue: Mutex<Requests<K, V>>,
    ready: Condvar,
}

struct Requests<K, V> {
    waiting: Vec<(K, V)>,
    serving: HashSet<K>,
}

impl<K: Clone + Eq + std::hash::Hash, V> Queue<K, V> {
    fn new() -> Self {
        Self {
            queue: Mutex::new(Requests {
                waiting: Vec::new(),
                serving: HashSet::new(),
            }),
            ready: Condvar::new(),
        }
    }

    /// A request still waiting for `key` is replaced and never answered.
    /// `Documents` relies on it: its answer to the newer request drops the
    /// older ones for the same slot from `requested`, which would
    /// otherwise wait forever.
    fn request(&self, key: K, value: V) {
        let mut queue = self.queue.lock().expect("queue lock");
        match queue.waiting.iter_mut().find(|(k, _)| *k == key) {
            Some(waiting) => waiting.1 = value,
            None => queue.waiting.push((key, value)),
        }
        self.ready.notify_one();
    }

    /// The oldest request for a key that isn't being served.
    fn next(&self) -> (K, V) {
        let mut queue = self.queue.lock().expect("queue lock");
        loop {
            let free = queue
                .waiting
                .iter()
                .position(|(key, _)| !queue.serving.contains(key));
            if let Some(at) = free {
                let (key, value) = queue.waiting.remove(at);
                queue.serving.insert(key.clone());
                return (key, value);
            }
            queue = self.ready.wait(queue).expect("queue lock");
        }
    }

    fn done(&self, key: &K) {
        self.queue.lock().expect("queue lock").serving.remove(key);
        self.ready.notify_all();
    }
}

/// Reads git facts on [`INSPECTORS`] threads.
type Inspector = Queue<SessionName, (PathBuf, Option<u64>)>;

fn spawn_inspectors(backend: &Arc<Backend>, msgs: &Sender<Msg>) -> Arc<Inspector> {
    let inspector = Arc::new(Inspector::new());
    for _ in 0..INSPECTORS {
        let inspector = Arc::clone(&inspector);
        let backend = Arc::clone(backend);
        let msgs = msgs.clone();
        thread::spawn(move || {
            loop {
                let (name, (worktree, asked_events)) = inspector.next();
                let read = backend
                    .vcs
                    .facts(&worktree)
                    .map(|facts| Inspected {
                        facts,
                        real_root: std::fs::canonicalize(&worktree).ok(),
                        asked_events,
                    })
                    .map_err(|e| describe(&e));
                inspector.done(&name);
                if msgs.send(Msg::Inspected { name, read }).is_err() {
                    return;
                }
            }
        });
    }
    inspector
}

/// Reads documents (diffs and branch logs) on [`DOCUMENT_READERS`] threads,
/// one per agent and file at a time.
type DocumentReader =
    Queue<(SessionName, Option<PathBuf>), (PathBuf, Key, Vec<(PathBuf, FileDiff)>)>;

fn spawn_document_readers(backend: &Arc<Backend>, msgs: &Sender<Msg>) -> Arc<DocumentReader> {
    let reader = Arc::new(DocumentReader::new());
    for _ in 0..DOCUMENT_READERS {
        let reader = Arc::clone(&reader);
        let backend = Arc::clone(backend);
        let msgs = msgs.clone();
        thread::spawn(move || {
            loop {
                let (at, (worktree, key, reuse)) = reader.next();
                let result = match &key {
                    Key::Diff(diff) => backend
                        .vcs
                        .file_diff(&worktree, &diff.change, diff.base)
                        .map(Doc::Diff),
                    Key::Branch(_) => backend.vcs.branch_log(&worktree).map(Doc::Branch),
                    Key::Commit(commit) => {
                        backend
                            .vcs
                            .commit_diff(&worktree, &commit.sha)
                            .map(|files| {
                                let changes: Vec<_> =
                                    files.iter().map(|(c, _)| c.clone()).collect();
                                let diffs = files.into_iter().map(|(c, d)| (c.path, d)).collect();
                                Doc::Folder(assemble(&changes, diffs, |_| Err("not read".into())))
                            })
                    }
                    Key::Folder(folder) => Ok(Doc::Folder(assemble(&folder.files, reuse, |c| {
                        backend
                            .vcs
                            .file_diff(&worktree, c, folder.base)
                            .map_err(|e| describe(&e))
                    }))),
                }
                .map_err(|e| describe(&e));
                reader.done(&at);
                if msgs.send(Msg::Document { key, result }).is_err() {
                    return;
                }
            }
        });
    }
    reader
}

/// The harness `name` runs, or the default one for an agent with no record.
fn harness_of<'a>(backend: &'a Backend, name: &SessionName) -> &'a dyn agent_core::ports::Harness {
    backend
        .store
        .record(name)
        .ok()
        .flatten()
        .and_then(|record| backend.harnesses.get(&record.harness))
        .unwrap_or_else(|| backend.harnesses.default_harness())
}

/// Reads subagent transcripts.
type SubagentReader = Queue<(SessionName, AgentId), PathBuf>;

/// Reads agents' current activities.
type ActivityReader = Queue<SessionName, (PathBuf, Option<TranscriptRef>)>;

/// Serves a queue of transcript reads on [`SUBAGENT_READERS`] threads.
/// `read` asks the harness of the session `session` names and says what came
/// back. The answer goes out before the key is free again, so a newer read of
/// the same key can't be answered first and then overwritten by this one.
fn spawn_log_readers<K, V>(
    backend: &Arc<Backend>,
    msgs: &Sender<Msg>,
    session: fn(&K) -> &SessionName,
    read: fn(&dyn agent_core::ports::Harness, &K, V) -> Msg,
) -> Arc<Queue<K, V>>
where
    K: Clone + Eq + std::hash::Hash + Send + 'static,
    V: Send + 'static,
{
    let reader = Arc::new(Queue::<K, V>::new());
    for _ in 0..SUBAGENT_READERS {
        let reader = Arc::clone(&reader);
        let backend = Arc::clone(backend);
        let msgs = msgs.clone();
        thread::spawn(move || {
            loop {
                let (key, value) = reader.next();
                let msg = read(harness_of(&backend, session(&key)), &key, value);
                let sent = msgs.send(msg);
                reader.done(&key);
                if sent.is_err() {
                    return;
                }
            }
        });
    }
    reader
}

fn spawn_subagent_readers(backend: &Arc<Backend>, msgs: &Sender<Msg>) -> Arc<SubagentReader> {
    spawn_log_readers(
        backend,
        msgs,
        |(name, _)| name,
        |harness, (name, id), workdir| Msg::Subagent {
            name: name.clone(),
            id: id.clone(),
            detail: harness.subagent(&workdir, id).map_err(|e| describe(&e)),
        },
    )
}

fn spawn_activity_readers(backend: &Arc<Backend>, msgs: &Sender<Msg>) -> Arc<ActivityReader> {
    spawn_log_readers(
        backend,
        msgs,
        |name| name,
        |harness, name, (workdir, at)| Msg::Activity {
            name: name.clone(),
            detail: harness
                .activity_detail(&workdir, at.as_ref())
                .map_err(|e| describe(&e)),
        },
    )
}

/// Asks the model to name prompts on one thread, one at a time. A model
/// call takes seconds, so the form's typing pauses must not each start one.
type Namer = Queue<(), (Prompt, HarnessId, u64)>;

fn spawn_namer(
    ask: impl Fn(&Prompt, &HarnessId) -> Option<AgentName> + Send + 'static,
    msgs: &Sender<Msg>,
) -> Arc<Namer> {
    let namer = Arc::new(Namer::new());
    let queue = Arc::clone(&namer);
    let msgs = msgs.clone();
    thread::spawn(move || {
        loop {
            let ((), (prompt, harness, generation)) = queue.next();
            let name = ask(&prompt, &harness);
            queue.done(&());
            if msgs.send(Msg::NameSuggested { generation, name }).is_err() {
                return;
            }
        }
    });
    namer
}

type Lookout = Queue<(), Look>;

/// What a scan for windows found, by session, or why it couldn't.
type Sightings = Result<HashMap<SessionName, Company>, String>;
type Scan = Arc<dyn Fn(Option<ClientId>) -> Sightings + Send + Sync>;

/// Looks for other windows on a thread each for the refresh's looks and for
/// the ones a decision waits on, the newest ask replacing
/// a waiting one.
struct Lookouts {
    refresh: Arc<Lookout>,
    decision: Arc<Lookout>,
}

impl Lookouts {
    fn request(&self, look: Look) {
        let queue = match look.need {
            Need::Refresh => &self.refresh,
            Need::Decision => &self.decision,
        };
        queue.request((), look);
    }
}

/// The company of every session with a window, from the host's windows.
fn companies(windows: Vec<Window>) -> HashMap<SessionName, Company> {
    let mut places: HashMap<SessionName, Vec<Place>> = HashMap::new();
    for window in windows {
        places.entry(window.session).or_default().push(window.place);
    }
    places
        .into_iter()
        .map(|(name, mut places)| {
            places.sort();
            (name, Company::of(places))
        })
        .collect()
}

fn host_scan(backend: &Arc<Backend>) -> Scan {
    let backend = Arc::clone(backend);
    Arc::new(move |own| {
        backend
            .services()
            .host
            .clients()
            .map_or(Ok(Vec::new()), |c| c.windows(own))
            .map(companies)
            .map_err(|e| describe(&e))
    })
}

fn spawn_lookouts(scan: &Scan, msgs: &Sender<Msg>) -> Lookouts {
    Lookouts {
        refresh: spawn_lookout(scan, msgs),
        decision: spawn_lookout(scan, msgs),
    }
}

fn spawn_lookout(scan: &Scan, msgs: &Sender<Msg>) -> Arc<Lookout> {
    let lookout = Arc::new(Lookout::new());
    let queue = Arc::clone(&lookout);
    let scan = Arc::clone(scan);
    let msgs = msgs.clone();
    thread::spawn(move || {
        loop {
            let ((), Look { own, ticket, .. }) = queue.next();
            let found = scan(own);
            queue.done(&());
            if msgs.send(Msg::Elsewhere { ticket, found }).is_err() {
                return;
            }
        }
    });
    lookout
}

/// Two threads: one refreshes the snapshot, coalescing requests; the other
/// runs user actions in order. A slow refresh never delays an action.
pub fn spawn(backend: Arc<Backend>, sources: Sources, msgs: Sender<Msg>) -> Workers {
    let (refresh, refresh_rx) = mpsc::channel::<()>();
    {
        let backend = Arc::clone(&backend);
        let msgs = msgs.clone();
        thread::spawn(move || {
            let mut heard = HashMap::new();
            let mut offered = (Vec::new(), Vec::new());
            loop {
                let msg = match refresh_rx.recv_timeout(ACTIVITY_POLL) {
                    Ok(()) => {
                        refresh_rx.try_iter().for_each(drop);
                        let catalogue = discover::catalogue(sources.dirs());
                        let warnings: Vec<String> =
                            catalogue.warnings.iter().map(ToString::to_string).collect();
                        if (&catalogue.repos, &warnings) != (&offered.0, &offered.1) {
                            offered = (catalogue.repos.clone(), warnings.clone());
                            let msg = Msg::Catalogue {
                                repos: catalogue.repos,
                                warnings,
                            };
                            if msgs.send(msg).is_err() {
                                return;
                            }
                        }
                        snapshot(&backend)
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => match backend.store.activities() {
                        Ok(activities) if activities != heard => {
                            heard.clone_from(&activities);
                            Msg::Activities(activities)
                        }
                        Ok(_) => continue,
                        Err(e) => Msg::Done(Err(describe(&e))),
                    },
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                if msgs.send(msg).is_err() {
                    return;
                }
            }
        });
    }

    let inspector = spawn_inspectors(&backend, &msgs);
    let subagents = spawn_subagent_readers(&backend, &msgs);
    let activities = spawn_activity_readers(&backend, &msgs);
    let documents = spawn_document_readers(&backend, &msgs);
    let lookouts = spawn_lookouts(&host_scan(&backend), &msgs);
    let namer = {
        let backend = Arc::clone(&backend);
        spawn_namer(
            move |prompt, harness| {
                let question = agent_core::naming::question(prompt);
                backend
                    .harnesses
                    .get(harness)
                    .and_then(|h| h.ask(&question).ok().flatten())
                    .as_deref()
                    .and_then(agent_core::naming::from_model)
            },
            &msgs,
        )
    };
    let (jobs, rx) = mpsc::channel::<Job>();
    let after = refresh.clone();
    let worker = thread::spawn(move || {
        for job in rx {
            let mutates = job.mutates();
            if let Some(msg) = run(&backend, job, &msgs)
                && msgs.send(msg).is_err()
            {
                return;
            }
            if mutates {
                let _ = after.send(());
            }
        }
    });
    Workers {
        jobs,
        inspector,
        subagents,
        activities,
        documents,
        namer,
        lookouts,
        refresh,
        worker: Some(worker),
    }
}

/// Starts the action detached from the TUI's terminal, reports that it
/// started, then waits for it alone (not for anything it backgrounds); a
/// failure comes back with its last line of stderr. Stderr goes to an
/// unlinked file rather than a pipe so the action never depends on the TUI
/// reading it, or dies writing to it after the TUI quits.
fn run_action(label: &str, launch: &CommandLine, msgs: &Sender<Msg>) -> Result<Outcome, String> {
    use std::io::{Read, Seek};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let failed = |e: std::io::Error| format!("{label}: {e}");
    if let Some(cwd) = launch.cwd.as_deref().filter(|cwd| !cwd.is_dir()) {
        return Err(format!("{label}: {} is gone", cwd.display()));
    }
    let mut log = tempfile::tempfile().map_err(failed)?;
    let mut command = Command::new(&launch.program);
    command.args(&launch.args);
    if let Some(cwd) = &launch.cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.try_clone().map_err(failed)?)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{label}: starting {}: {e}", launch.program.display()))?;
    let _ = msgs.send(Msg::Done(Ok(Outcome::Started(label.to_string()))));
    let status = child.wait().map_err(failed)?;
    if status.success() {
        return Ok(Outcome::Finished(label.to_string()));
    }
    let mut stderr = String::new();
    let _ = log
        .rewind()
        .and_then(|()| log.take(STDERR_TAIL).read_to_string(&mut stderr));
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    Err(format!(
        "{label} failed ({status}){}",
        if last.is_empty() {
            String::new()
        } else {
            format!(": {last}")
        }
    ))
}

fn snapshot(backend: &Backend) -> Msg {
    Msg::Snapshot(
        ops::snapshot(backend.services())
            .map(|(snapshot, warnings)| {
                (snapshot, warnings.iter().map(ToString::to_string).collect())
            })
            .map_err(|e| describe(&e)),
    )
}

/// Runs `job`, returning its reply; an action replies later from its own
/// thread instead.
fn run(backend: &Arc<Backend>, job: Job, msgs: &Sender<Msg>) -> Option<Msg> {
    let svc = backend.services();
    let err = |e: ops::Error| describe(&e);
    Some(match job {
        Job::CheckDir(path) => {
            let found = if path.is_dir() {
                Found::Dir
            } else {
                Found::Nothing
            };
            Msg::DirChecked { path, found }
        }
        Job::Run { label, launch } => {
            let msgs = msgs.clone();
            thread::spawn(move || {
                let _ = msgs.send(Msg::Done(run_action(&label, &launch, &msgs)));
            });
            return None;
        }
        Job::Start {
            at,
            harness,
            ticket,
        } => {
            let result = svc
                .harness(&harness)
                .and_then(|h| ops::start(svc, &at, h, None))
                .map_err(err);
            Msg::Launched { ticket, result }
        }
        Job::Launch {
            launch,
            harness,
            ticket,
        } => Msg::Launched {
            ticket,
            result: launch_session(svc, launch, &harness).map_err(err),
        },
        Job::Reply {
            to,
            prompt,
            not_before,
        } => {
            let result = reply(svc, &to, &prompt, not_before);
            Msg::Sent { to, result }
        }
        Job::Kill(running) => Msg::Done(
            ops::kill(svc, &running)
                .map(|()| Outcome::Killed(running.name().clone()))
                .map_err(err),
        ),
        Job::PlanRemoval {
            target,
            ticket,
            own,
        } => Msg::RemovalPlanned {
            ticket,
            result: ops::plan_removal(svc, target, own.as_ref()).map_err(err),
        },
        Job::Remove { approved, own } => Msg::Done(remove(svc, approved, own.as_ref())),
        Job::CloseElsewhere { name, own } => Msg::ClosedElsewhere {
            closed: close_elsewhere(svc, &name, own).map_err(|e| describe(&e)),
            name,
        },
        Job::Acknowledge(name) => return report_failure(svc.store.acknowledge(&name)),
        Job::Select(name) => return report_failure(svc.store.set_last_selected(&name)),
        Job::Pin(name) => Msg::Done(
            svc.store
                .pin(&name)
                .map(|()| Outcome::Pinned(name))
                .map_err(|e| describe(&e)),
        ),
        Job::Unpin(name) => Msg::Done(
            svc.store
                .unpin(&name)
                .map(|()| Outcome::Unpinned(name))
                .map_err(|e| describe(&e)),
        ),
        Job::ListRepo(dir) => {
            Msg::Done(ops::list_repo(svc, &dir).map(Outcome::Listed).map_err(err))
        }
    })
}

/// What a job with nothing to report unless it failed replies.
fn report_failure<E: std::error::Error + 'static>(result: Result<(), E>) -> Option<Msg> {
    result.err().map(|e| Msg::Done(Err(describe(&e))))
}

fn launch_session(
    svc: ops::Services<'_>,
    launch: Launchable,
    harness: &HarnessId,
) -> Result<ops::Launch, ops::Error> {
    let repo_of = |choice: RepoChoice| match choice {
        RepoChoice::Known(root) => Ok(root),
        RepoChoice::Path(path) => ops::repo_for(svc, &path)?.ok_or(ops::Error::NotARepo(path)),
    };
    match launch {
        Launchable::Create { name, repo, task } => ops::create(
            svc,
            &name,
            &repo_of(repo)?,
            task.as_ref(),
            svc.harness(harness)?,
        ),
        Launchable::Attach { name, task } => ops::join(svc, &name.session(), task.as_ref()),
        Launchable::Checkout { name, repo, target } => ops::checkout_launch(
            svc,
            &repo_of(repo)?,
            &target,
            Some(name),
            svc.harness(harness)?,
        ),
        Launchable::Resurrect(name) => ops::join(svc, &name.session(), None),
        Launchable::Start { at, task } => ops::start(svc, &at, svc.harness(harness)?, task),
    }
}

fn reply(
    svc: ops::Services<'_>,
    to: &SessionName,
    prompt: &Prompt,
    not_before: Option<SystemTime>,
) -> Result<(), SendError> {
    match svc.store.activities() {
        Ok(all)
            if all
                .get(to)
                .is_some_and(|a| a.ready_for_text(not_before, SystemTime::now())) =>
        {
            ops::inject(svc, to, prompt).map_err(|e| SendError::Failed(describe(&e)))
        }
        Ok(_) => Err(SendError::Busy),
        Err(e) => Err(SendError::Unreadable(describe(&e))),
    }
}

fn remove(
    svc: ops::Services<'_>,
    approved: Approved,
    own: Option<&SessionName>,
) -> Result<Outcome, String> {
    match ops::remove(svc, approved, own).map_err(|e| describe(&e))? {
        Removed::Session(name) => Ok(Outcome::Removed {
            name,
            warnings: Vec::new(),
        }),
        Removed::Agent {
            worktree,
            mut warnings,
            ..
        } => {
            let name = worktree.name().session();
            if let Err(e) = ops::spawn_sweep(svc, &[worktree]) {
                warnings.push(describe(&e));
            }
            Ok(Outcome::Removed { name, warnings })
        }
    }
}

fn close_elsewhere(
    svc: ops::Services<'_>,
    name: &SessionName,
    own: Option<ClientId>,
) -> Result<usize, HostError> {
    let Some(clients) = svc.host.clients() else {
        return Ok(0);
    };
    let others: Vec<ClientId> = clients
        .windows(own)?
        .into_iter()
        .filter(|w| w.session == *name)
        .map(|w| w.client)
        .collect();
    clients.detach(&others)?;
    Ok(others.len())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(5);

    fn ticket_of(reply: &Msg) -> Ticket {
        match reply {
            Msg::Elsewhere { ticket, .. } => *ticket,
            _ => panic!("a look's answer"),
        }
    }

    #[test]
    fn a_prompt_look_is_answered_while_a_refresh_scan_is_still_running() {
        let (msgs, replies) = mpsc::channel();
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let calls = Arc::new(AtomicUsize::new(0));
        let scan: Scan = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let _ = gate.lock().expect("gate").recv_timeout(PATIENCE);
                }
                Ok(HashMap::new())
            })
        };
        let lookouts = spawn_lookouts(&scan, &msgs);
        let mut m = crate::fixtures::fleet(1);
        let (slow, quick) = (m.start_look(), m.start_look());
        let look = |ticket, need| Look {
            own: None,
            ticket,
            need,
        };
        lookouts.request(look(slow, Need::Refresh));
        let started = Instant::now();
        while calls.load(Ordering::SeqCst) == 0 {
            assert!(started.elapsed() < PATIENCE, "the refresh scan never began");
            thread::sleep(Duration::from_millis(1));
        }
        lookouts.request(look(quick, Need::Decision));
        let first = replies.recv_timeout(PATIENCE).expect("an answer");
        assert_eq!(
            ticket_of(&first),
            quick,
            "the refresh scan is still blocked"
        );
        release.send(()).expect("released");
        let second = replies.recv_timeout(PATIENCE).expect("the refresh answer");
        assert_eq!(ticket_of(&second), slow);
    }

    #[test]
    fn windows_are_grouped_by_session_with_their_places_sorted() {
        let window = |client, session: &str, place| Window {
            client: ClientId::new(client),
            session: session.parse().expect("a name"),
            place,
        };
        let found = companies(vec![
            window(3, "fix", Place::Ssh),
            window(1, "fix", Place::Nvim),
            window(2, "other", Place::Tmux),
        ]);
        let of = |name: &str| found.get(&name.parse::<SessionName>().expect("a name"));
        assert_eq!(of("fix"), Some(&Company::of(vec![Place::Nvim, Place::Ssh])));
        assert_eq!(of("other"), Some(&Company::of(vec![Place::Tmux])));
        assert_eq!(of("idle"), None);
    }

    #[test]
    fn at_most_one_name_is_asked_for_at_a_time_and_only_the_latest_waiting_prompt_is() {
        let (msgs, replies) = mpsc::channel();
        let (started, running) = mpsc::channel::<u64>();
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let asking = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let namer = {
            let (asking, most) = (Arc::clone(&asking), Arc::clone(&most));
            spawn_namer(
                move |prompt, _| {
                    let now = asking.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    let _ = started.send(prompt.as_str().len() as u64);
                    gate.lock()
                        .expect("gate lock")
                        .recv_timeout(PATIENCE)
                        .expect("released");
                    asking.fetch_sub(1, Ordering::SeqCst);
                    None
                },
                &msgs,
            )
        };
        let harness: HarnessId = "claude".parse().expect("parses");
        let ask = |generation: u64, text: &str| {
            namer.request(
                (),
                (text.parse().expect("a prompt"), harness.clone(), generation),
            );
        };

        ask(1, "first");
        running.recv_timeout(PATIENCE).expect("the first is asked");
        ask(2, "second one");
        ask(3, "third one here");
        assert!(
            running.recv_timeout(Duration::from_millis(100)).is_err(),
            "nothing else is asked while one is in flight"
        );

        release.send(()).expect("open");
        assert_eq!(
            running.recv_timeout(PATIENCE).expect("the latest is asked"),
            "third one here".len() as u64,
            "the prompt that was replaced is never asked"
        );
        release.send(()).expect("open");

        let generations: Vec<u64> = (0..2)
            .map(|_| match replies.recv_timeout(PATIENCE).expect("replies") {
                Msg::NameSuggested { generation, .. } => generation,
                _ => panic!("a name was expected"),
            })
            .collect();
        assert_eq!(generations, [1, 3]);
        assert_eq!(most.load(Ordering::SeqCst), 1, "never two at once");
    }
}
