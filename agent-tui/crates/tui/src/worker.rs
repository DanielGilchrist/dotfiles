use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use agent_core::actions::CommandLine;
use agent_core::activity::Activity;
use agent_core::model::{Removable, Running, Snapshot, Startable};
use agent_core::names::{AgentName, Prompt, SessionName};
use agent_core::ops::{self, Approved, Backend, Launch, RemovalPlan, Removed};
use agent_core::ports::{ClientId, HarnessId, HostError, WorktreeFacts};
use crossterm::event::{KeyEvent, MouseEvent};

use crate::model::{Launchable, RepoChoice, Ticket};

/// How much of a failed action's stderr is read for its last line.
const STDERR_TAIL: u64 = 64 * 1024;

/// Threads reading git facts, so a slow repo holds up only its own agents.
const INSPECTORS: usize = 2;

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
    Shared {
        name: SessionName,
        others: usize,
    },
    /// The harness's model named the prompt of `generation` (or couldn't).
    NameSuggested {
        generation: u64,
        name: Option<AgentName>,
    },
    TookOver {
        name: SessionName,
        detached: Result<usize, String>,
    },
    /// The terminal gained or lost focus.
    Focus(crate::model::TerminalFocus),
    /// What every agent's harness last reported, when it changed.
    Activities(HashMap<SessionName, Activity>),
    /// Git facts about an agent's worktree.
    Inspected {
        name: SessionName,
        facts: Result<WorktreeFacts, String>,
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
    /// A configured action's process started.
    Started(String),
    /// It exited successfully.
    Finished(String),
    Replied(SessionName),
}

pub enum Job {
    /// Type a reply into an agent and submit it.
    Reply {
        to: SessionName,
        prompt: Prompt,
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
    CheckShared {
        name: SessionName,
        own: Option<ClientId>,
    },
    /// Remember the selection for the next start.
    Select(SessionName),
    /// Ask the harness's model for a name; runs on its own thread.
    SuggestName {
        prompt: Prompt,
        harness: HarnessId,
        generation: u64,
    },
    TakeOver {
        name: SessionName,
        own: Option<ClientId>,
    },
    /// The user saw an agent's finished turn.
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
            | Self::Unpin(_) => true,
            Self::PlanRemoval { .. }
            | Self::CheckShared { .. }
            | Self::Select(_)
            | Self::SuggestName { .. }
            | Self::TakeOver { .. }
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
    pub refresh: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Workers {
    /// Asks for git facts about an agent's worktree.
    pub fn inspect(&self, name: SessionName, worktree: PathBuf) {
        self.inspector.request(name, worktree);
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

/// Reads git facts on [`INSPECTORS`] threads, one request per agent at a
/// time. A request for an agent already being read waits its turn, so the
/// answer reflects its latest state.
struct Inspector {
    queue: Mutex<Inspections>,
    ready: Condvar,
}

#[derive(Default)]
struct Inspections {
    waiting: Vec<(SessionName, PathBuf)>,
    reading: HashSet<SessionName>,
}

impl Inspector {
    fn spawn(backend: &Arc<Backend>, msgs: &Sender<Msg>) -> Arc<Self> {
        let inspector = Arc::new(Self {
            queue: Mutex::new(Inspections::default()),
            ready: Condvar::new(),
        });
        for _ in 0..INSPECTORS {
            let inspector = Arc::clone(&inspector);
            let backend = Arc::clone(backend);
            let msgs = msgs.clone();
            thread::spawn(move || {
                loop {
                    let (name, worktree) = inspector.next();
                    let facts = backend.vcs.facts(&worktree).map_err(|e| describe(&e));
                    inspector.done(&name);
                    if msgs.send(Msg::Inspected { name, facts }).is_err() {
                        return;
                    }
                }
            });
        }
        inspector
    }

    fn request(&self, name: SessionName, worktree: PathBuf) {
        let mut queue = self.queue.lock().expect("inspections lock");
        match queue.waiting.iter_mut().find(|(n, _)| *n == name) {
            Some(waiting) => waiting.1 = worktree,
            None => queue.waiting.push((name, worktree)),
        }
        self.ready.notify_one();
    }

    /// The oldest request for an agent that isn't being read.
    fn next(&self) -> (SessionName, PathBuf) {
        let mut queue = self.queue.lock().expect("inspections lock");
        loop {
            let free = queue
                .waiting
                .iter()
                .position(|(name, _)| !queue.reading.contains(name));
            if let Some(at) = free {
                let (name, worktree) = queue.waiting.remove(at);
                queue.reading.insert(name.clone());
                return (name, worktree);
            }
            queue = self.ready.wait(queue).expect("inspections lock");
        }
    }

    fn done(&self, name: &SessionName) {
        self.queue
            .lock()
            .expect("inspections lock")
            .reading
            .remove(name);
        self.ready.notify_all();
    }
}

/// Two threads: one refreshes the snapshot, coalescing requests; the other
/// runs user actions in order. A slow refresh never delays an action.
pub fn spawn(backend: Arc<Backend>, msgs: Sender<Msg>) -> Workers {
    let (refresh, refresh_rx) = mpsc::channel::<()>();
    {
        let backend = Arc::clone(&backend);
        let msgs = msgs.clone();
        thread::spawn(move || {
            let mut heard = HashMap::new();
            loop {
                let msg = match refresh_rx.recv_timeout(ACTIVITY_POLL) {
                    Ok(()) => {
                        refresh_rx.try_iter().for_each(drop);
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

    let inspector = Inspector::spawn(&backend, &msgs);
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

/// Runs `job`, returning its reply; a model call for a name may take
/// seconds, so it replies later from its own thread instead.
fn run(backend: &Arc<Backend>, job: Job, msgs: &Sender<Msg>) -> Option<Msg> {
    let svc = backend.services();
    let err = |e: ops::Error| describe(&e);
    Some(match job {
        Job::SuggestName {
            prompt,
            harness,
            generation,
        } => {
            let backend = Arc::clone(backend);
            let msgs = msgs.clone();
            thread::spawn(move || {
                let question = agent_core::naming::question(&prompt);
                let reply = backend
                    .harnesses
                    .get(&harness)
                    .and_then(|h| h.ask(&question).ok().flatten());
                let name = reply.as_deref().and_then(agent_core::naming::from_model);
                let _ = msgs.send(Msg::NameSuggested { generation, name });
            });
            return None;
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
        } => {
            let result = (|| match launch {
                Launchable::Create { name, repo, task } => {
                    let repo = match repo {
                        RepoChoice::Known(root) => root,
                        RepoChoice::Path(path) => {
                            ops::repo_for(svc, &path)?.ok_or(ops::Error::NotARepo(path))?
                        }
                    };
                    ops::create(svc, &name, &repo, task.as_ref(), svc.harness(&harness)?)
                }
                Launchable::Attach { name, task } => ops::join(svc, &name.session(), task.as_ref()),
                Launchable::Resurrect(name) => ops::join(svc, &name.session(), None),
                Launchable::Start { at, task } => {
                    ops::start(svc, &at, svc.harness(&harness)?, task)
                }
            })();
            Msg::Launched {
                ticket,
                result: result.map_err(err),
            }
        }
        Job::Acknowledge(name) => match svc.store.acknowledge(&name) {
            Ok(()) => return None,
            Err(e) => Msg::Done(Err(describe(&e))),
        },
        Job::Reply { to, prompt } => Msg::Done(match svc.store.activities() {
            Ok(all) if all.get(&to).is_some_and(Activity::at_rest) => {
                ops::inject(svc, &to, &prompt)
                    .map(|()| Outcome::Replied(to))
                    .map_err(err)
            }
            Ok(_) => Err(format!("{to} started working, so the reply wasn't sent")),
            Err(e) => Err(describe(&e)),
        }),
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
        Job::Remove { approved, own } => {
            Msg::Done(match ops::remove(svc, approved, own.as_ref()) {
                Ok(Removed::Session(name)) => Ok(Outcome::Removed {
                    name,
                    warnings: Vec::new(),
                }),
                Ok(Removed::Agent {
                    worktree,
                    mut warnings,
                    ..
                }) => {
                    let name = worktree.name().session();
                    if let Err(e) = ops::spawn_sweep(svc, &[worktree]) {
                        warnings.push(describe(&e));
                    }
                    Ok(Outcome::Removed { name, warnings })
                }
                Err(e) => Err(err(e)),
            })
        }
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
        Job::Select(name) => match svc.store.set_last_selected(&name) {
            Ok(()) => return None,
            Err(e) => Msg::Done(Err(describe(&e))),
        },
        Job::CheckShared { name, own } => {
            let others = svc
                .host
                .clients()
                .map_or(Ok(0), |c| c.other_clients(&name, own).map(|v| v.len()));
            Msg::Shared {
                name,
                others: others.unwrap_or(0),
            }
        }
        Job::TakeOver { name, own } => {
            let detached = svc.host.clients().map_or(Ok(0), |c| {
                let others = c.other_clients(&name, own)?;
                c.detach(&others)?;
                Ok(others.len())
            });
            Msg::TookOver {
                name,
                detached: detached.map_err(|e: HostError| describe(&e)),
            }
        }
    })
}
