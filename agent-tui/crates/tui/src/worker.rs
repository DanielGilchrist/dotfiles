use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use agent_core::actions::CommandLine;
use agent_core::activity::Activity;
use agent_core::model::{Removable, Running, Snapshot, Startable};
use agent_core::names::{AgentName, Prompt, SessionName};
use agent_core::ops::{self, Approved, Backend, Launch, RemovalPlan, Removed};
use agent_core::ports::{ClientId, HarnessId, HostError, Progress};
use crossterm::event::{KeyEvent, MouseEvent};

use crate::model::{Launchable, RepoChoice, Ticket};

/// How much of a failed action's stderr is read for its last line.
const STDERR_TAIL: u64 = 64 * 1024;

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
    /// Git facts about an agent's worktree, for the peek.
    Inspected {
        name: SessionName,
        facts: Result<Facts, String>,
    },
}

/// An agent's worktree as the peek shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub progress: Progress,
    pub dirty: usize,
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
}

pub enum Job {
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
    /// Read git facts about an agent's worktree; runs on its own thread.
    Inspect {
        name: SessionName,
        worktree: std::path::PathBuf,
    },
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
            | Self::Inspect { .. }
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
    pub refresh: Sender<()>,
    worker: Option<JoinHandle<()>>,
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
        Job::Inspect { name, worktree } => {
            let backend = Arc::clone(backend);
            let msgs = msgs.clone();
            thread::spawn(move || {
                let vcs = &backend.vcs;
                let facts = vcs
                    .progress(&worktree)
                    .and_then(|progress| {
                        Ok(Facts {
                            progress,
                            dirty: vcs.dirty_files(&worktree)?,
                        })
                    })
                    .map_err(|e| describe(&e));
                let _ = msgs.send(Msg::Inspected { name, facts });
            });
            return None;
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
