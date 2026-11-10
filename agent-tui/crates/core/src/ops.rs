use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::activity::{Hold, Join, MainEvent, Move, Reported, Standing, TranscriptRef};
use crate::discover::{self, Warning};
use crate::model::{HostedRepo, HostedWorktree, Presence, Removable, Running, Snapshot, Startable};
use crate::names::{
    AgentName, BranchName, Checkout, InvalidPrompt, NameError, NameLimit, Prompt, SessionName,
};
use crate::paths::{AgentWorktree, Layout, PathError, RepoRoot, TrashSlot, WorktreeState};
use crate::ports::{
    AgentRecord, BranchState, Fetch, Forge, Harness, HarnessError, HarnessId, Harnesses, HostError,
    LaunchCtx, Listing, PaneCommand, PendingPrompt, Probe, SessionHost, SessionInfo, SessionStatus,
    Store, StoreError, TranscriptStamp, Unlisting, Vcs, WorktreeStart,
};
use crate::process::{CommandError, LaunchSpec};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error(transparent)]
    Prompt(#[from] InvalidPrompt),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("multiple worktrees are named {name}. Remove the duplicates first: {}", display_paths(.paths))]
    Ambiguous {
        name: AgentName,
        paths: Vec<PathBuf>,
    },
    #[error("no agent or session named {0}")]
    NotFound(SessionName),
    #[error("{0} is still being created. Try again in a moment")]
    Creating(AgentName),
    #[error("not inside a git repository (pass --repo)")]
    NoRepo,
    #[error("{0} is not inside a git repository")]
    NotARepo(PathBuf),
    #[error("{0} can't be listed: paths must be UTF-8 without newlines")]
    Unstorable(PathBuf),
    #[error("{0} already exists. Move it aside first")]
    Occupied(PathBuf),
    #[error("{name}'s worktree {} is gone. Run this from its repo to recreate it", .path.display())]
    WorktreeGone { name: SessionName, path: PathBuf },
    #[error("{0} isn't running, so the prompt can't be delivered. Attach first, then send it")]
    NotRunning(SessionName),
    #[error("nothing sent: {} · {}", .hold.says(.name), .hold.hint())]
    Held { name: SessionName, hold: Hold },
    #[error("would lose work: {}", describe_hazards(.0))]
    Hazards(Vec<Hazard>),
    #[error("{0} is the session this runs in. Remove it from somewhere else")]
    OwnSession(SessionName),
    #[error("{0} changed since the removal was confirmed. Check it again")]
    Changed(SessionName),
    #[error("{0} appeared since it was checked. Try again")]
    Appeared(AgentName),
    #[error("{branch} is checked out at {}, outside the agent worktrees", .path.display())]
    CheckedOutElsewhere { branch: BranchName, path: PathBuf },
    #[error("branch {0} not found locally or on origin")]
    BranchNotFound(BranchName),
    #[error("{name} is already an agent on {}", .on.as_ref().map_or("a detached HEAD".to_string(), ToString::to_string))]
    NameTaken {
        name: AgentName,
        on: Option<BranchName>,
    },
    #[error("no agent record for {0}, so agent didn't start it")]
    NoRecord(SessionName),
    #[error("unknown harness {0}")]
    UnknownHarness(HarnessId),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn io_error(context: impl Into<String>) -> impl FnOnce(io::Error) -> Error {
    move |source| Error::Io {
        context: context.into(),
        source,
    }
}

/// Owned ports, for callers that outlive a borrow (the TUI's worker thread).
pub struct Backend {
    pub vcs: Box<dyn Vcs>,
    pub forge: Box<dyn Forge>,
    pub host: Box<dyn SessionHost>,
    pub store: Box<dyn Store>,
    pub harnesses: Box<dyn Harnesses>,
    pub layout: Layout,
    pub exe: PathBuf,
    /// The session this process runs in, read once at startup.
    pub own: Option<SessionName>,
}

impl Backend {
    pub fn services(&self) -> Services<'_> {
        Services {
            vcs: self.vcs.as_ref(),
            forge: self.forge.as_ref(),
            host: self.host.as_ref(),
            store: self.store.as_ref(),
            harnesses: self.harnesses.as_ref(),
            layout: &self.layout,
            exe: &self.exe,
            own: self.own.as_ref(),
        }
    }
}

/// The ports a use case runs against, plus the binary to put in pane commands.
#[derive(Clone, Copy)]
pub struct Services<'a> {
    pub vcs: &'a dyn Vcs,
    pub forge: &'a dyn Forge,
    pub host: &'a dyn SessionHost,
    pub store: &'a dyn Store,
    pub harnesses: &'a dyn Harnesses,
    pub layout: &'a Layout,
    pub exe: &'a Path,
    pub own: Option<&'a SessionName>,
}

impl Services<'_> {
    pub fn harness(&self, id: &HarnessId) -> Result<&dyn Harness> {
        self.harnesses
            .get(id)
            .ok_or_else(|| Error::UnknownHarness(id.clone()))
    }
}

/// The first prompt for an agent in a fresh, detached worktree.
pub fn bootstrap_prompt(task: Option<&Prompt>) -> Prompt {
    let intro: Prompt = "This worktree is detached at the repo's default branch. Once you understand the task, \
                         create a branch with `git checkout -b <kebab-case-name>` named for it before making any \
                         changes."
        .parse()
        .expect("the intro is a valid prompt");
    match task {
        Some(task) => intro
            .then(&"Task:".parse().expect("a valid prompt"))
            .then(task),
        None => intro,
    }
}

pub fn snapshot(svc: Services<'_>) -> Result<(Snapshot, Vec<Warning>)> {
    let listed = svc.store.repos()?;
    let mut obs = discover::observe(svc.vcs, svc.host, svc.layout, &listed)?;
    let pins = svc.store.pinned()?;
    let (snapshot, mut warnings) =
        discover::assemble(&obs, svc.layout, &pins, svc.host.name_limit());
    warnings.append(&mut obs.warnings);
    Ok((snapshot, warnings))
}

pub fn repo_for(svc: Services<'_>, dir: &Path) -> Result<Option<RepoRoot>> {
    Ok(svc.vcs.repo_of(dir)?)
}

/// Keeps the repo containing `dir` in the list, so the TUI shows it with
/// nothing running in it. The repo must be nameable as a session, or it
/// could never show.
pub fn list_repo(svc: Services<'_>, dir: &Path) -> Result<Listed> {
    let repo = repo_for(svc, dir)?.ok_or_else(|| Error::NotARepo(dir.to_path_buf()))?;
    listable(svc, &repo)?;
    let listing = svc.store.list_repo(&repo)?;
    Ok(Listed { repo, listing })
}

/// Why `repo` couldn't be listed: its name can't be a session, or its
/// path can't be stored.
fn listable(svc: Services<'_>, repo: &RepoRoot) -> Result<HostedRepo> {
    let hosted = HostedRepo::admit(svc.host.name_limit(), repo.clone())?;
    if repo.path().to_str().is_none_or(|p| p.contains('\n')) {
        return Err(Error::Unstorable(repo.path().to_path_buf()));
    }
    Ok(hosted)
}

/// What [`list_repo`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub repo: RepoRoot,
    pub listing: Listing,
}

/// Drops `dir` from the list: as written when it's listed that way (so a
/// path that isn't a repo any more can be forgotten), else the repo
/// containing it. The repo stays discovered while sessions or agent
/// worktrees belong to it.
pub fn unlist_repo(svc: Services<'_>, dir: &Path) -> Result<Unlisted> {
    let as_written = RepoRoot::new(dir.to_path_buf())?;
    let repo = if svc.store.repos()?.contains(&as_written) {
        as_written
    } else {
        repo_for(svc, dir)?.unwrap_or(as_written)
    };
    let unlisting = svc.store.unlist_repo(&repo)?;
    Ok(Unlisted { repo, unlisting })
}

/// What [`unlist_repo`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlisted {
    pub repo: RepoRoot,
    pub unlisting: Unlisting,
}

fn find_session(svc: Services<'_>, name: &SessionName) -> Result<Option<SessionInfo>> {
    Ok(svc.host.sessions()?.into_iter().find(|s| &s.name == name))
}

/// The one agent worktree named `name`, if any.
/// One git is still creating is an error rather than absent, so nothing
/// tries to start or replace it. An unlinked one is still the agent's,
/// and an empty directory counts as absent.
fn find_worktree(svc: Services<'_>, name: &AgentName) -> Result<Option<AgentWorktree>> {
    let found = svc.layout.named(name).map_err(io_error(format!(
        "scanning {}",
        svc.layout.root().display()
    )))?;
    let mut ready: Vec<AgentWorktree> = Vec::new();
    for (wt, state) in found {
        match state {
            WorktreeState::Ready | WorktreeState::Unlinked => ready.push(wt),
            WorktreeState::Initialising => return Err(Error::Creating(name.clone())),
            WorktreeState::Gone => {}
        }
    }
    if ready.len() > 1 {
        let paths = ready.iter().map(|wt| wt.path().to_path_buf()).collect();
        return Err(Error::Ambiguous {
            name: name.clone(),
            paths,
        });
    }
    Ok(ready.pop())
}

pub(crate) struct AttachFacts<'a> {
    pub name: &'a AgentName,
    pub existing: Option<&'a AgentWorktree>,
    pub session: Option<&'a SessionInfo>,
    pub repo: Option<&'a RepoRoot>,
    pub layout: &'a Layout,
    pub limit: NameLimit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// The session exists. Attach, resurrecting it if it exited.
    Attach {
        session: SessionName,
        presence: Presence,
        cwd: Option<PathBuf>,
    },
    Start(Start),
    /// The session is alive but its worktree is gone.
    Recreate {
        worktree: HostedWorktree,
        repo: RepoRoot,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Start {
    Existing(Startable),
    New {
        worktree: HostedWorktree,
        repo: RepoRoot,
    },
}

/// How to reach an agent. Only [`plan`] makes these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachPlan(Plan);

impl AttachPlan {
    pub fn session(&self) -> SessionName {
        match &self.0 {
            Plan::Attach { session, .. } => session.clone(),
            Plan::Start(Start::Existing(at)) => at.name(),
            Plan::Start(Start::New { worktree, .. }) | Plan::Recreate { worktree, .. } => {
                worktree.host().session().clone()
            }
        }
    }

    /// The worktree this will create, where a prompt would be the first
    /// message.
    pub fn new_worktree(&self) -> Option<&AgentWorktree> {
        match &self.0 {
            Plan::Start(Start::New { worktree, .. }) => Some(worktree.at()),
            _ => None,
        }
    }
}

pub(crate) fn plan_attach(facts: &AttachFacts<'_>) -> Result<AttachPlan> {
    let plan = match (facts.existing, facts.session) {
        // An exited agent starts again through `agent exec` rather than
        // whatever command the session host cached, so it gets today's launch.
        (
            Some(wt),
            None
            | Some(SessionInfo {
                status: SessionStatus::Exited { .. },
                ..
            }),
        ) => {
            let hosted = HostedWorktree::admit(facts.limit, wt.clone())?;
            Plan::Start(Start::Existing(Startable::worktree(hosted)))
        }
        (Some(wt), Some(s)) => Plan::Attach {
            session: s.name.clone(),
            presence: s.status.presence(),
            cwd: Some(wt.path().to_path_buf()),
        },
        (None, Some(s)) => match &s.status {
            SessionStatus::Exited { .. } => Plan::Attach {
                session: s.name.clone(),
                presence: Presence::Exited,
                cwd: None,
            },
            SessionStatus::Live { cwd: Some(cwd) } => {
                match facts
                    .layout
                    .enclosing(cwd)
                    .filter(|gone| gone.name() == facts.name)
                {
                    Some(gone) => {
                        let own_repo = facts
                            .repo
                            .filter(|r| facts.layout.worktree(r, facts.name.clone()) == gone);
                        let repo = own_repo.ok_or_else(|| Error::WorktreeGone {
                            name: s.name.clone(),
                            path: gone.path().to_path_buf(),
                        })?;
                        Plan::Recreate {
                            worktree: HostedWorktree::admit(facts.limit, gone)?,
                            repo: repo.clone(),
                        }
                    }
                    None => Plan::Attach {
                        session: s.name.clone(),
                        presence: Presence::Live,
                        cwd: Some(cwd.clone()),
                    },
                }
            }
            SessionStatus::Live { cwd: None } => Plan::Attach {
                session: s.name.clone(),
                presence: Presence::Live,
                cwd: None,
            },
        },
        (None, None) => {
            let repo = facts.repo.ok_or(Error::NoRepo)?;
            Plan::Start(Start::New {
                worktree: HostedWorktree::admit(
                    facts.limit,
                    facts.layout.worktree(repo, facts.name.clone()),
                )?,
                repo: repo.clone(),
            })
        }
    };
    Ok(AttachPlan(plan))
}

/// What happened to the task passed to [`execute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    None,
    /// Typed into the running agent.
    Injected,
    /// Passed to the harness when the agent next launches.
    AtLaunch,
}

/// What to run in a terminal to reach an agent.
#[derive(Debug, Clone)]
pub struct Launch {
    pub session: SessionName,
    pub spec: LaunchSpec,
    pub delivery: Delivery,
}

/// Gathers the facts for `name` and plans how to reach it.
pub fn plan(svc: Services<'_>, name: &AgentName, repo: Option<&RepoRoot>) -> Result<AttachPlan> {
    let existing = find_worktree(svc, name)?;
    let session = find_session(svc, &name.session())?;
    plan_attach(&AttachFacts {
        name,
        existing: existing.as_ref(),
        session: session.as_ref(),
        repo,
        layout: svc.layout,
        limit: svc.host.name_limit(),
    })
}

/// `git worktree add`, removing whatever it left behind if it fails.
fn create_worktree(
    svc: Services<'_>,
    repo: &RepoRoot,
    new: &HostedWorktree,
    start: &WorktreeStart,
) -> Result<()> {
    let at = new.at();
    if at.path().exists() {
        return Err(Error::Occupied(at.path().to_path_buf()));
    }
    svc.vcs.prune_worktrees(repo)?;
    if let Err(e) = svc.vcs.add_worktree(repo, at, start) {
        if at.path().exists() {
            let _ = std::fs::remove_dir_all(at.path());
        }
        let _ = svc.vcs.prune_worktrees(repo);
        return Err(e.into());
    }
    Ok(())
}

/// Proof that the store showed nothing holding text typed into a session.
/// Only [`check_typeable`] makes one, and only [`type_into`] takes it.
struct Clear<'a> {
    name: &'a SessionName,
}

/// Refuses with [`Error::Held`] when the store says text typed into `name`
/// could land in a menu. A session with no activity record at all, one agent
/// didn't start, has nothing to refuse on.
fn check_typeable<'a>(svc: Services<'_>, name: &'a SessionName) -> Result<Clear<'a>> {
    let activities = svc.store.activities()?;
    match activities.get(name).and_then(|a| a.hold(SystemTime::now())) {
        Some(hold) => Err(Error::Held {
            name: name.clone(),
            hold,
        }),
        None => Ok(Clear { name }),
    }
}

/// Types `prompt` into the session `clear` is for, using its harness's input
/// encoding. Sessions agent didn't start get the default harness's.
fn type_into(svc: Services<'_>, clear: &Clear<'_>, prompt: &Prompt) -> Result<()> {
    let harness = harness_for(svc, clear.name)?;
    svc.host.write(clear.name, &harness.submit(prompt))?;
    Ok(())
}

/// Types `prompt` into a running session unless the store shows a menu
/// could take it.
pub fn inject(svc: Services<'_>, name: &SessionName, prompt: &Prompt) -> Result<()> {
    type_into(svc, &check_typeable(svc, name)?, prompt)
}

/// The harness `name` runs, or the default one for a session agent didn't
/// start.
pub fn harness_for<'a>(svc: Services<'a>, name: &SessionName) -> Result<&'a dyn Harness> {
    match svc.store.record(name)? {
        Some(record) => svc
            .harnesses
            .get(&record.harness)
            .ok_or(Error::UnknownHarness(record.harness)),
        None => Ok(svc.harnesses.default_harness()),
    }
}

/// Reads `name`'s transcript for what `standing` asks, and records the move
/// the answer makes, if it makes one. Nothing is read while `seen` still
/// describes the file.
pub fn probe_interrupt(
    svc: Services<'_>,
    name: &SessionName,
    at: &TranscriptRef,
    standing: Standing,
    seen: Option<&TranscriptStamp>,
) -> Result<(Probe, Option<Move>)> {
    let probe = harness_for(svc, name)?.interruption(at, standing.after(), seen)?;
    let moved = standing.moved_by(&probe);
    match moved {
        Some(Move::Interrupted(proof, at)) => svc.store.interrupt(name, proof, at)?,
        Some(Move::Resumed(proof)) => svc.store.resume(name, proof)?,
        None => {}
    }
    Ok((probe, moved))
}

fn deliver(svc: Services<'_>, session: &SessionName, task: Option<&Prompt>) -> Result<Delivery> {
    match task {
        Some(task) => {
            inject(svc, session, task)?;
            Ok(Delivery::Injected)
        }
        None => Ok(Delivery::None),
    }
}

/// Records that `name` is about to launch, so a record left by an earlier
/// run reads as booting from before the session can be listed. `exec_spec`
/// refines it once the harness is started.
fn mark_launch(svc: Services<'_>, name: &SessionName) -> Result<()> {
    svc.store
        .report(name, Reported::main(MainEvent::Launched(Join::Untraced)))?;
    Ok(())
}

/// Marks a resurrection when `agent` started the session. One it didn't
/// start runs no `agent exec`, so nothing would ever refine or end the mark
/// and the session's missing record, which tells the TUI it can't know what
/// is open in it, would be replaced by a false one.
fn mark_resurrection(svc: Services<'_>, name: &SessionName) -> Result<()> {
    if svc.store.record(name)?.is_some() {
        mark_launch(svc, name)?;
    }
    Ok(())
}

/// Attaches to the exited session `session`, which resurrects and runs
/// `agent exec` again.
pub fn resurrect(svc: Services<'_>, session: &SessionName) -> Result<Launch> {
    mark_resurrection(svc, session)?;
    join(svc, session, None)
}

/// Attaches to the existing session `session` (resurrecting it if it
/// exited), typing `task` into it first.
pub fn join(svc: Services<'_>, session: &SessionName, task: Option<&Prompt>) -> Result<Launch> {
    let delivery = deliver(svc, session, task)?;
    Ok(Launch {
        session: session.clone(),
        spec: svc.host.attach(session),
        delivery,
    })
}

/// Creates the agent `name` in `repo`, refusing if anything has taken the
/// name since the caller checked it was free.
pub fn create(
    svc: Services<'_>,
    name: &AgentName,
    repo: &RepoRoot,
    task: Option<&Prompt>,
    harness: &dyn Harness,
) -> Result<Launch> {
    let plan = plan(svc, name, Some(repo))?;
    match plan.new_worktree() {
        Some(_) => {
            // The worktree makes the repo show either way, so one that can't
            // be listed just doesn't stay once the agent is gone.
            if listable(svc, repo).is_ok() {
                svc.store.list_repo(repo)?;
            }
            execute(svc, plan, task, harness)
        }
        None => Err(Error::Appeared(name.clone())),
    }
}

/// Carries out an [`AttachPlan`]. `task` is typed into a running agent or
/// passed at launch to a new one. It's an error if neither is possible. `harness`
/// only applies to agents that don't already have one.
pub fn execute(
    svc: Services<'_>,
    plan: AttachPlan,
    task: Option<&Prompt>,
    harness: &dyn Harness,
) -> Result<Launch> {
    let session = plan.session();
    match plan.0 {
        Plan::Attach {
            presence: Presence::Exited,
            ..
        } if task.is_some() => Err(Error::NotRunning(session)),
        Plan::Attach { cwd, presence, .. } => {
            if presence == Presence::Exited {
                mark_resurrection(svc, &session)?;
            }
            let delivery = deliver(svc, &session, task)?;
            let spec = svc.host.attach(&session);
            let spec = match cwd {
                Some(cwd) => spec.cwd(cwd),
                None => spec,
            };
            Ok(Launch {
                session,
                spec,
                delivery,
            })
        }
        // The session is live while git checks the files out again, so the
        // TUI lists it as an orphan until the next refresh after this.
        Plan::Recreate { worktree, repo } => {
            let clear = task.map(|_| check_typeable(svc, &session)).transpose()?;
            create_worktree(svc, &repo, &worktree, &WorktreeStart::DefaultBase)?;
            let delivery = match clear.zip(task) {
                Some((clear, task)) => {
                    type_into(svc, &clear, task)?;
                    Delivery::Injected
                }
                None => Delivery::None,
            };
            Ok(Launch {
                spec: svc.host.attach(&session).cwd(worktree.at().path()),
                session,
                delivery,
            })
        }
        Plan::Start(Start::New { worktree, repo }) => {
            create_worktree(svc, &repo, &worktree, &WorktreeStart::DefaultBase)?;
            let at = Startable::worktree(worktree);
            start(svc, &at, harness, Some(bootstrap_prompt(task)))
        }
        Plan::Start(Start::Existing(at)) => start(svc, &at, harness, task.cloned()),
    }
}

pub fn attach(
    svc: Services<'_>,
    name: &AgentName,
    task: Option<&Prompt>,
    repo: Option<&RepoRoot>,
    harness: &dyn Harness,
) -> Result<Launch> {
    execute(svc, plan(svc, name, repo)?, task, harness)
}

/// Start a session whose pane runs `agent exec`, which asks the agent's
/// harness to resume or launch. An agent that already has a record keeps
/// its harness. `prompt` is passed at the next launch.
pub fn start(
    svc: Services<'_>,
    at: &Startable,
    harness: &dyn Harness,
    prompt: Option<Prompt>,
) -> Result<Launch> {
    let name = at.name();
    let existing = svc.store.record(&name)?;
    let harness_id = existing
        .as_ref()
        .map_or_else(|| harness.id().clone(), |r| r.harness.clone());
    svc.harness(&harness_id)?;
    let (delivery, pending) = match prompt {
        Some(prompt) => (Delivery::AtLaunch, Some(PendingPrompt::now(prompt))),
        None => (Delivery::None, None),
    };

    let pane = PaneCommand {
        program: svc.exe.into(),
        args: vec!["exec".into(), name.to_string()],
        cwd: at.cwd().into(),
    };
    mark_launch(svc, &name)?;
    let spec = match svc.host.create(at.host_name(), &pane) {
        Ok(spec) => spec,
        Err(e) => {
            let _ = svc.store.report(&name, Reported::main(MainEvent::Ended));
            return Err(e.into());
        }
    };
    svc.store.put_record(
        &name,
        &AgentRecord {
            harness: harness_id,
            pending,
        },
    )?;
    Ok(Launch {
        session: name,
        spec,
        delivery,
    })
}

/// Attach to the session for `repo`'s main checkout, starting it if there
/// isn't one (named after the repo, running `harness`). Either way the
/// repo joins the list, so it stays shown once the session is gone.
pub fn open_repo(svc: Services<'_>, repo: &RepoRoot, harness: &dyn Harness) -> Result<Launch> {
    let at = Startable::main(listable(svc, repo)?);
    let name = at.name();
    let launch = match find_session(svc, &name)?.map(|s| s.status) {
        Some(SessionStatus::Live { .. }) => Launch {
            spec: svc.host.attach(&name).cwd(repo.path()),
            session: name,
            delivery: Delivery::None,
        },
        Some(SessionStatus::Exited { .. }) | None => start(svc, &at, harness, None)?,
    };
    svc.store.list_repo(repo)?;
    Ok(launch)
}

/// Attach to the agent whose worktree is `worktree`, starting its session
/// if it exited or never began. Nothing is created: a worktree without a
/// checkout is reported, not replaced.
pub fn attach_here(
    svc: Services<'_>,
    worktree: &AgentWorktree,
    harness: &dyn Harness,
) -> Result<Launch> {
    match worktree.state() {
        WorktreeState::Ready => {}
        WorktreeState::Initialising => return Err(Error::Creating(worktree.name().clone())),
        WorktreeState::Unlinked | WorktreeState::Gone => {
            return Err(Error::WorktreeGone {
                name: worktree.name().session(),
                path: worktree.path().to_path_buf(),
            });
        }
    }
    let session = find_session(svc, &worktree.name().session())?;
    let plan = plan_attach(&AttachFacts {
        name: worktree.name(),
        existing: Some(worktree),
        session: session.as_ref(),
        repo: None,
        layout: svc.layout,
        limit: svc.host.name_limit(),
    })?;
    execute(svc, plan, None, harness)
}

/// The harness command for a session's pane. A pending prompt is passed until
/// the agent shows activity after it was recorded, then dropped, so a crash
/// before the agent saw it retries and a resurrection doesn't replay it.
pub fn exec_spec(svc: Services<'_>, name: &SessionName, workdir: &Path) -> Result<LaunchSpec> {
    let mut record = svc
        .store
        .record(name)?
        .ok_or_else(|| Error::NoRecord(name.clone()))?;
    let harness = svc.harness(&record.harness)?;
    // If activity can't be read, keep the prompt: resending beats losing it.
    if let Some(seen) = record
        .pending
        .as_ref()
        .and_then(|p| harness.acknowledged(workdir, p).ok().flatten())
    {
        record.acknowledge(seen);
        svc.store.put_record(name, &record)?;
    }
    let bootstrap = record.pending.as_ref().map(|p| &p.prompt);
    let spec = harness.launch(&LaunchCtx {
        session: name,
        workdir,
        bootstrap,
        report: svc.exe,
    })?;
    // The harness replaces this process, keeping its pid. A failed write
    // must not stop the launch.
    let _ = svc
        .store
        .report(name, Reported::main(MainEvent::Launched(spec.join())));
    Ok(spec)
}

/// Ensure an agent worktree exists for `branch`. Returns its name.
pub fn checkout(
    svc: Services<'_>,
    repo: &RepoRoot,
    branch: &BranchName,
    name: Option<AgentName>,
) -> Result<AgentName> {
    checkout_from(svc, repo, &Fetch::Branch(branch.clone()), name)
}

/// The agent for `fetch`'s branch in `repo`: the worktree already on it,
/// or a new one named `name` (else after the branch). A branch is taken
/// from the local branch, else origin's, fetched if unknown. A pull
/// request head is always fetched, since origin's copy may be stale or
/// another branch's altogether, and reset onto its `pr/N` branch.
fn checkout_from(
    svc: Services<'_>,
    repo: &RepoRoot,
    fetch: &Fetch,
    name: Option<AgentName>,
) -> Result<AgentName> {
    let branch = fetch.branch();
    let worktrees = svc.vcs.worktrees(repo)?;
    if worktrees.main.head.branch() == Some(&branch) {
        return Err(Error::CheckedOutElsewhere {
            branch,
            path: worktrees.main.path,
        });
    }
    if let Some(wt) = worktrees
        .linked
        .iter()
        .find(|wt| wt.head.branch() == Some(&branch))
    {
        return match svc.layout.worktree_at(&wt.path) {
            Some(aw) => Ok(aw.name().clone()),
            None => Err(Error::CheckedOutElsewhere {
                branch,
                path: wt.path.clone(),
            }),
        };
    }
    let name = match name {
        Some(name) => name,
        None => AgentName::from_branch(&branch, svc.host.name_limit())?,
    };
    let at = svc.layout.worktree(repo, name.clone());
    if let Some(existing) = find_worktree(svc, &name)? {
        // Only this repo's own path can be the branch's worktree: another
        // repo's branches are its own. The scan above misses this path
        // only when git can't read it.
        if existing.path() != at.path() {
            return Err(Error::NameTaken {
                name,
                on: svc
                    .vcs
                    .head(existing.path())
                    .ok()
                    .and_then(|h| h.branch().cloned()),
            });
        }
        let Ok(head) = svc.vcs.head(existing.path()) else {
            return Err(Error::Occupied(existing.path().to_path_buf()));
        };
        if head.branch() == Some(&branch) {
            return Ok(name);
        }
        return Err(Error::NameTaken {
            name,
            on: head.branch().cloned(),
        });
    }
    let new = HostedWorktree::admit(svc.host.name_limit(), at)?;
    let start = match fetch {
        Fetch::PullRequest(_) => {
            svc.vcs.fetch(repo, fetch)?;
            match svc.vcs.remote_branch(repo, &branch)? {
                Some(remote) => WorktreeStart::AtRemote(remote),
                None => return Err(Error::BranchNotFound(branch)),
            }
        }
        Fetch::Branch(_) => {
            if let Some(local) = svc.vcs.local_branch(repo, &branch)? {
                WorktreeStart::Branch(local)
            } else {
                let mut remote = svc.vcs.remote_branch(repo, &branch)?;
                if remote.is_none() {
                    svc.vcs.fetch(repo, fetch)?;
                    remote = svc.vcs.remote_branch(repo, &branch)?;
                }
                match remote {
                    Some(remote) => WorktreeStart::TrackRemote(remote),
                    None => return Err(Error::BranchNotFound(branch)),
                }
            }
        }
    };
    create_worktree(svc, repo, &new, &start)?;
    if listable(svc, repo).is_ok() {
        svc.store.list_repo(repo)?;
    }
    Ok(name)
}

/// Checks `target` out as the agent `name` in `repo` and plans reaching
/// it with no prompt, as `checkout` then `attach` would. A pull request
/// from the repo itself is its branch, so the agent can push to it. One
/// from a fork is its head under `pr/N`.
pub fn checkout_launch(
    svc: Services<'_>,
    repo: &RepoRoot,
    target: &Checkout,
    name: Option<AgentName>,
    harness: &dyn Harness,
) -> Result<Launch> {
    let fetch = match target {
        Checkout::Branch(branch) => Fetch::Branch(branch.clone()),
        Checkout::PullRequest(pr) => {
            let head = svc.forge.pull_request_head(pr)?;
            if head.cross_repository {
                Fetch::PullRequest(pr.number)
            } else {
                Fetch::Branch(head.branch)
            }
        }
    };
    let name = checkout_from(svc, repo, &fetch, name)?;
    let plan = plan(svc, &name, Some(repo))?;
    execute(svc, plan, None, harness)
}

/// Something a removal would destroy that exists nowhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hazard {
    /// Modified, staged or untracked files.
    Uncommitted { files: NonZeroUsize },
    /// Commits on a detached HEAD that no branch or remote holds.
    Detached { commits: NonZeroUsize },
    Unmerged {
        branch: BranchName,
        ahead: NonZeroUsize,
    },
    /// The branch's upstream was deleted, but later commits can't be ruled out.
    UpstreamGone {
        branch: BranchName,
        ahead: NonZeroUsize,
    },
    /// No default branch to compare against.
    Unverifiable { branch: BranchName },
}

impl fmt::Display for Hazard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plural = |n: NonZeroUsize, one: &str, many: &str| {
            format!("{n} {}", if n.get() == 1 { one } else { many })
        };
        match self {
            Self::Uncommitted { files } => write!(
                f,
                "{}",
                plural(*files, "uncommitted file", "uncommitted files")
            ),
            Self::Detached { commits } => {
                write!(f, "{} on no branch", plural(*commits, "commit", "commits"))
            }
            Self::Unmerged { branch, ahead } => write!(
                f,
                "{branch} has {}",
                plural(*ahead, "unmerged commit", "unmerged commits")
            ),
            Self::UpstreamGone { branch, ahead } => {
                write!(
                    f,
                    "{branch} has {} and its upstream is gone",
                    plural(*ahead, "commit", "commits")
                )
            }
            Self::Unverifiable { branch } => write!(f, "can't tell whether {branch} is merged"),
        }
    }
}

fn describe_hazards(hazards: &[Hazard]) -> String {
    hazards
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Everything in `worktree` that removing it would lose.
fn hazards(
    svc: Services<'_>,
    repo: &RepoRoot,
    worktree: &AgentWorktree,
    branch: Option<&BranchName>,
) -> Result<Vec<Hazard>> {
    let mut found = Vec::new();
    if let Some(files) = NonZeroUsize::new(svc.vcs.dirty_files(worktree.path())?) {
        found.push(Hazard::Uncommitted { files });
    }
    match branch {
        None => {
            if let Some(commits) = NonZeroUsize::new(svc.vcs.unreachable_commits(worktree.path())?)
            {
                found.push(Hazard::Detached { commits });
            }
        }
        Some(branch) => match svc.vcs.branch_state(repo, branch)? {
            BranchState::Missing | BranchState::Safe => {}
            BranchState::Unmerged { ahead } => found.push(Hazard::Unmerged {
                branch: branch.clone(),
                ahead,
            }),
            BranchState::UpstreamGone { ahead } => found.push(Hazard::UpstreamGone {
                branch: branch.clone(),
                ahead,
            }),
            BranchState::Unknown => found.push(Hazard::Unverifiable {
                branch: branch.clone(),
            }),
        },
    }
    Ok(found)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Agent {
        worktree: AgentWorktree,
        repo: RepoRoot,
        branch: Option<BranchName>,
    },
    Session(SessionName),
}

/// What removing an agent would do, as seen when it was planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalPlan {
    target: Target,
    /// Its session, if running, for killing it alone instead.
    running: Option<Running>,
    hazards: Vec<Hazard>,
}

/// A plan that would lose something. Only this can be forced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hazardous(Box<RemovalPlan>);

impl Hazardous {
    pub fn plan(&self) -> &RemovalPlan {
        &self.0
    }

    /// Go ahead despite the hazards the user saw.
    pub fn force(self) -> Approved {
        Approved(Box::new(Approval::Forced(*self.0)))
    }
}

/// What a removal deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deletes<'a> {
    /// The session only: there's no worktree or branch.
    Session,
    /// The session, the worktree and its branch, if it's on one.
    Agent {
        worktree: &'a AgentWorktree,
        branch: Option<&'a BranchName>,
    },
}

/// What removal checks again just before deleting anything.
enum Recheck {
    /// Refuse if anything could now be lost.
    Hazards,
    /// The user accepted the losses, so only refuse if the branch changed.
    BranchOnly,
}

/// A removal the user has agreed to. Only [`RemovalPlan::approve`] and
/// [`Hazardous::force`] make one.
#[derive(Debug)]
pub struct Approved(Box<Approval>);

#[derive(Debug)]
enum Approval {
    /// Refused at removal time if anything changed or could be lost.
    Clean(RemovalPlan),
    /// Goes ahead despite the hazards the user saw, but still refused if the
    /// worktree switched branch since.
    Forced(RemovalPlan),
}

/// What the user agreed to lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Nothing: refuse if anything would be lost.
    Clean,
    /// Unmerged commits and uncommitted files.
    Discard,
}

impl Approved {
    pub fn plan(&self) -> &RemovalPlan {
        match &*self.0 {
            Approval::Clean(plan) | Approval::Forced(plan) => plan,
        }
    }

    pub fn name(&self) -> SessionName {
        self.plan().name()
    }
}

impl RemovalPlan {
    pub fn name(&self) -> SessionName {
        match &self.target {
            Target::Agent { worktree, .. } => worktree.name().session(),
            Target::Session(name) => name.clone(),
        }
    }

    pub fn hazards(&self) -> &[Hazard] {
        &self.hazards
    }

    /// What removal deletes, for describing it.
    pub fn deletes(&self) -> Deletes<'_> {
        match &self.target {
            Target::Agent {
                worktree, branch, ..
            } => Deletes::Agent {
                worktree,
                branch: branch.as_ref(),
            },
            Target::Session(_) => Deletes::Session,
        }
    }

    pub fn running(&self) -> Option<&Running> {
        self.running.as_ref()
    }

    /// Approval without forcing, refused when anything could be lost.
    pub fn approve(self) -> Result<Approved, Hazardous> {
        if self.hazards.is_empty() {
            Ok(Approved(Box::new(Approval::Clean(self))))
        } else {
            Err(Hazardous(Box::new(self)))
        }
    }
}

/// What `name` refers to for removal: its agent worktree if it has one,
/// otherwise its session.
pub fn removable(svc: Services<'_>, name: &SessionName) -> Result<Removable> {
    if let Ok(agent) = name.as_str().parse::<AgentName>()
        && let Some(worktree) = find_worktree(svc, &agent)?
    {
        return Ok(Removable::Worktree(worktree));
    }
    match find_session(svc, name)? {
        Some(_) => Ok(Removable::Session(name.clone())),
        None => Err(Error::NotFound(name.clone())),
    }
}

/// A removal planned for a session other than the caller's own, the only
/// kind [`remove`] can be handed.
#[derive(Debug)]
pub enum Planned {
    Elsewhere(RemovalPlan),
    Own(OwnPlan),
}

impl Planned {
    /// The plan, or [`Error::OwnSession`] when it targets the caller's own session.
    pub fn elsewhere(self) -> Result<RemovalPlan> {
        match self {
            Self::Elsewhere(plan) => Ok(plan),
            Self::Own(own) => Err(Error::OwnSession(own.name())),
        }
    }
}

/// A removal of the session the caller runs in. It can't be approved here
/// because stopping the session would kill the removal halfway.
#[derive(Debug)]
pub struct OwnPlan(RemovalPlan);

impl OwnPlan {
    pub fn name(&self) -> SessionName {
        self.0.name()
    }

    pub fn hazards(&self) -> &[Hazard] {
        self.0.hazards()
    }
}

/// The session this process runs in is never offered for killing, and
/// removing it is a [`Planned::Own`].
pub fn plan_removal(svc: Services<'_>, target: Removable) -> Result<Planned> {
    let plan = plan_target(svc, target, svc.own)?;
    Ok(if svc.own == Some(&plan.name()) {
        Planned::Own(OwnPlan(plan))
    } else {
        Planned::Elsewhere(plan)
    })
}

fn plan_target(
    svc: Services<'_>,
    target: Removable,
    own: Option<&SessionName>,
) -> Result<RemovalPlan> {
    let running = |session: Option<SessionInfo>| match session {
        Some(SessionInfo {
            name,
            status: SessionStatus::Live { .. },
        }) if own != Some(&name) => Some(Running::new(name)),
        _ => None,
    };
    match target {
        Removable::Session(name) => {
            let session = find_session(svc, &name)?.ok_or_else(|| Error::NotFound(name.clone()))?;
            Ok(RemovalPlan {
                target: Target::Session(name),
                running: running(Some(session)),
                hazards: Vec::new(),
            })
        }
        Removable::Worktree(worktree) => {
            let running = running(find_session(svc, &worktree.name().session())?);
            let repo = svc
                .vcs
                .repo_of(worktree.path())?
                .ok_or_else(|| Error::NotARepo(worktree.path().to_path_buf()))?;
            let branch = svc.vcs.head(worktree.path())?.into_branch();
            let hazards = hazards(svc, &repo, &worktree, branch.as_ref())?;
            Ok(RemovalPlan {
                target: Target::Agent {
                    worktree,
                    repo,
                    branch,
                },
                running,
                hazards,
            })
        }
    }
}

#[derive(Debug)]
pub enum Removed {
    Session(SessionName),
    /// The worktree was moved into the trash. [`sweep`] finishes deleting it.
    /// Later cleanup steps that failed are reported, not fatal.
    Agent {
        worktree: AgentWorktree,
        trashed: TrashSlot,
        warnings: Vec<String>,
    },
}

pub fn remove(svc: Services<'_>, approved: Approved) -> Result<Removed> {
    let (plan, recheck) = match *approved.0 {
        Approval::Clean(plan) => (plan, Recheck::Hazards),
        Approval::Forced(plan) => (plan, Recheck::BranchOnly),
    };
    let name = plan.name();
    let (worktree, repo, planned) = match plan.target {
        Target::Session(name) => {
            svc.host.delete(&name)?;
            svc.store.forget(&name)?;
            return Ok(Removed::Session(name));
        }
        Target::Agent {
            worktree,
            repo,
            branch,
        } => (worktree, repo, branch),
    };

    // Stop the agent (resurrectably) so nothing changes under the final
    // check. Only once the worktree is safely in the trash is the session
    // deleted for good. The session may have been started since the plan.
    let stopped = match find_session(svc, &name)?.map(|s| s.status) {
        Some(SessionStatus::Live { .. }) => {
            svc.host.kill(&Running::new(name.clone()))?;
            Some(&name)
        }
        Some(SessionStatus::Exited { .. }) => Some(&name),
        None => None,
    };
    let branch = svc.vcs.head(worktree.path())?.into_branch();
    if branch != planned {
        return Err(Error::Changed(name));
    }
    if let Recheck::Hazards = recheck {
        let found = hazards(svc, &repo, &worktree, branch.as_ref())?;
        if !found.is_empty() {
            return Err(Error::Hazards(found));
        }
    }

    let trash = svc.layout.trash();
    std::fs::create_dir_all(&trash).map_err(io_error(format!("creating {}", trash.display())))?;
    let slot = svc.layout.trash_slot(worktree.name());
    std::fs::rename(worktree.path(), slot.path()).map_err(io_error(format!(
        "moving {} to the trash",
        worktree.path().display()
    )))?;

    let mut warnings = Vec::new();
    let mut warn = |step: &str, result: Result<()>| {
        if let Err(e) = result {
            warnings.push(format!("{step}: {e}"));
        }
    };
    if let Some(session) = stopped {
        warn(
            "deleting the session",
            svc.host.delete(session).map_err(Error::from),
        );
    }
    warn(
        "pruning worktrees",
        svc.vcs.prune_worktrees(&repo).map_err(Error::from),
    );
    if let Some(branch) = &branch {
        let delete = svc
            .vcs
            .local_branch(&repo, branch)
            .and_then(|found| match found {
                Some(local) => svc.vcs.delete_branch(&repo, local),
                None => Ok(()),
            });
        warn(&format!("deleting {branch}"), delete.map_err(Error::from));
    }
    warn(
        "forgetting the agent",
        svc.store.forget(&name).map_err(Error::from),
    );
    if let Some(parent) = worktree.path().parent()
        && let Err(e) = std::fs::remove_dir(parent)
        && !matches!(
            e.kind(),
            io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
        )
    {
        warnings.push(format!("removing {}: {e}", parent.display()));
    }
    Ok(Removed::Agent {
        worktree,
        trashed: slot,
        warnings,
    })
}

/// Stop a session. It can be resurrected later.
pub fn kill(svc: Services<'_>, session: &Running) -> Result<()> {
    svc.host.kill(session)?;
    Ok(())
}

/// Delete everything in the trash, and any `removed` worktree directories
/// that came back without a `.git` (editor tooling rooted in a removed
/// worktree recreates cache dirs for a few seconds).
pub fn sweep(layout: &Layout, removed: &[AgentWorktree]) -> io::Result<()> {
    match std::fs::read_dir(layout.trash()) {
        Ok(entries) => {
            for entry in entries {
                std::fs::remove_dir_all(entry?.path())?;
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    for worktree in removed {
        let path = worktree.path();
        if path.exists() && !path.join(".git").exists() {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

/// Run [`sweep`] in a detached `agent sweep` process, which outlives the
/// caller and waits out editor tooling first.
pub fn spawn_sweep(svc: Services<'_>, removed: &[AgentWorktree]) -> Result<()> {
    Command::new(svc.exe)
        .arg("sweep")
        .args(removed.iter().map(AgentWorktree::path))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io_error("starting the background sweep"))?;
    Ok(())
}

/// Agent worktrees that can be removed without losing anything.
pub fn merged(svc: Services<'_>) -> Result<Vec<(AgentWorktree, Option<BranchName>)>> {
    let mut clean = Vec::new();
    for worktree in svc.layout.scan().map_err(io_error("scanning worktrees"))? {
        let Some(repo) = svc.vcs.repo_of(worktree.path())? else {
            continue;
        };
        let branch = svc.vcs.head(worktree.path())?.into_branch();
        if hazards(svc, &repo, &worktree, branch.as_ref())?.is_empty() {
            clean.push((worktree, branch));
        }
    }
    Ok(clean)
}

#[cfg(test)]
mod tests;
