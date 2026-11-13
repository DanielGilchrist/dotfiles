use std::time::{Duration, SystemTime};

use super::{Error, Own, Planned, Result, Services, find_session, snapshot, start_then};
use crate::activity::{Activity, BackgroundCommands, Busy, counted_commands};
use crate::model::{Presence, Running, Startable};
use crate::names::SessionName;
use crate::ports::{HarnessId, Lookup, SessionStatus, SurvivorState};

/// How long to wait for something that settles on its own, as polls `every`.
#[derive(Debug, Clone, Copy)]
pub struct Patience {
    pub polls: u32,
    pub every: Duration,
}

impl Patience {
    /// The session host lists a session as gone or running a moment after
    /// it was killed or started, and a harness takes a moment to notice its
    /// terminal went and to register the process as one it can stop.
    pub const SETTLE: Self = Self {
        polls: 60,
        every: Duration::from_millis(50),
    };

    /// Polls `done` until it says yes, which is whether it did within the
    /// time.
    fn wait(self, mut done: impl FnMut() -> Result<bool>) -> Result<bool> {
        for poll in 0..=self.polls {
            if done()? {
                return Ok(true);
            }
            if poll < self.polls {
                std::thread::sleep(self.every);
            }
        }
        Ok(false)
    }
}

/// What a restart does with an agent whose session already exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExited {
    /// Start it again, like attaching to it.
    Start,
    /// Only stop the harness process it left running, and keep it exited.
    Leave,
}

/// Where a restart leaves the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Started,
    LeftExited,
}

impl End {
    /// What happened to the agent `name`, run by a harness called `label`.
    pub fn says(self, name: &SessionName, label: &str) -> String {
        match self {
            Self::Started => format!("restarted {name}"),
            Self::LeftExited => format!("stopped {label} for {name} · stays exited"),
        }
    }
}

/// What a restart would interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupts {
    /// The session's own activity.
    Session(Busy),
    /// A process the harness runs in the background, mid-task.
    Survivor,
}

impl Interrupts {
    pub fn label(self) -> &'static str {
        match self {
            Self::Session(busy) => busy.label(),
            Self::Survivor => "working in the background",
        }
    }

    /// What an agent approved as idle did since.
    pub fn became(self) -> String {
        match self {
            Self::Session(busy) => busy.became(),
            Self::Survivor => "started working in the background".to_string(),
        }
    }
}

/// What a restart would stop: the agent's activity, the background commands
/// its harness runs, or both, and never neither. A restart that stops
/// something is only approved through this, so the user has seen it.
///
/// Restarting stops the harness process, which may take the commands it
/// started down with it, so they are what it may stop, not what it will.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stops {
    Activity(Interrupts),
    Commands(BackgroundCommands),
    Both(Interrupts, BackgroundCommands),
}

/// `2 background commands`.
fn counted(commands: &BackgroundCommands) -> String {
    counted_commands(commands.count().get())
}

impl Stops {
    fn of(activity: Option<Interrupts>, commands: Option<BackgroundCommands>) -> Option<Self> {
        match (activity, commands) {
            (None, None) => None,
            (Some(activity), None) => Some(Self::Activity(activity)),
            (None, Some(commands)) => Some(Self::Commands(commands)),
            (Some(activity), Some(commands)) => Some(Self::Both(activity, commands)),
        }
    }

    pub fn activity(&self) -> Option<Interrupts> {
        match self {
            Self::Activity(activity) | Self::Both(activity, _) => Some(*activity),
            Self::Commands(_) => None,
        }
    }

    /// Every command it may stop, including ones that can't be named on
    /// screen.
    pub fn commands(&self) -> Option<&BackgroundCommands> {
        match self {
            Self::Commands(commands) | Self::Both(_, commands) => Some(commands),
            Self::Activity(_) => None,
        }
    }

    /// What `self`, as found at restart time, would stop beyond what the
    /// user was `shown`, or nothing when that covers it. Any activity covers
    /// any activity, and commands are covered one by one.
    fn beyond(&self, shown: Option<&Self>) -> Option<Self> {
        let Some(shown) = shown else {
            return Some(self.clone());
        };
        let activity = self.activity().filter(|_| shown.activity().is_none());
        let commands = self.commands().and_then(|fresh| match shown.commands() {
            None => Some(fresh.clone()),
            Some(known) => fresh.not_in(known),
        });
        Self::of(activity, commands)
    }

    /// As `working and running 2 background commands`.
    pub fn label(&self) -> String {
        self.label_naming(|activity| activity.label().to_string())
    }

    /// [`label`](Self::label) with the activity worded by `activity`.
    pub fn label_naming(&self, activity: impl FnOnce(Interrupts) -> String) -> String {
        match self {
            Self::Activity(interrupts) => activity(*interrupts),
            Self::Commands(commands) => format!("running {}", counted(commands)),
            Self::Both(interrupts, commands) => {
                format!(
                    "{} and running {}",
                    activity(*interrupts),
                    counted(commands)
                )
            }
        }
    }

    /// What an agent approved without this did since, when `self` is what
    /// wasn't shown.
    pub fn became(&self) -> String {
        match self {
            Self::Activity(activity) => activity.became(),
            Self::Commands(commands) => format!("started running {}", counted(commands)),
            Self::Both(activity, commands) => {
                format!(
                    "{} and started running {}",
                    activity.became(),
                    counted(commands)
                )
            }
        }
    }
}

/// What restarting an agent would do, as seen when it was planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPlan {
    at: Startable,
    harness: HarnessId,
    label: &'static str,
    was: Was,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Was {
    Live {
        /// For killing it first.
        running: Running,
    },
    /// Exited or never started, with the process its harness left running.
    Down {
        survivor: Option<SurvivorState>,
        then: OnExited,
    },
}

/// What is known of restarting the caller's own session, which can't be
/// done from inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnRestart {
    stops: Option<Stops>,
}

impl OwnRestart {
    pub fn stops(&self) -> Option<&Stops> {
        self.stops.as_ref()
    }
}

impl RestartPlan {
    pub fn name(&self) -> SessionName {
        self.at.name()
    }

    pub fn end(&self) -> End {
        match &self.was {
            Was::Live { .. }
            | Was::Down {
                then: OnExited::Start,
                ..
            } => End::Started,
            Was::Down {
                then: OnExited::Leave,
                ..
            } => End::LeftExited,
        }
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Whether an exited agent has no process left, so there is nothing for
    /// a fleet restart to do to it.
    fn has_nothing_to_stop(&self) -> bool {
        matches!(
            self.was,
            Was::Down {
                survivor: None,
                then: OnExited::Leave,
            }
        )
    }
}

/// What a plan came to: a restart nothing stands in the way of, or one the
/// user must see what it would stop to approve.
#[derive(Debug)]
pub enum RestartApproval {
    Free(Restart),
    Needs(WouldStop),
}

impl RestartApproval {
    pub fn plan(&self) -> &RestartPlan {
        match self {
            Self::Free(go) => go.plan(),
            Self::Needs(would_stop) => would_stop.plan(),
        }
    }

    /// What it would stop, if anything.
    pub fn stops(&self) -> Option<&Stops> {
        match self {
            Self::Free(_) => None,
            Self::Needs(would_stop) => Some(would_stop.stops()),
        }
    }

    /// Go ahead, which for a restart that would stop something is a force.
    pub fn proceed(self) -> Restart {
        match self {
            Self::Free(go) => go,
            Self::Needs(would_stop) => would_stop.force(),
        }
    }
}

/// A restart that would stop something. Only this can be forced.
#[derive(Debug)]
pub struct WouldStop {
    plan: Box<RestartPlan>,
    stops: Box<Stops>,
}

impl WouldStop {
    pub fn plan(&self) -> &RestartPlan {
        &self.plan
    }

    pub fn stops(&self) -> &Stops {
        &self.stops
    }

    /// Go ahead despite what the user saw it would stop. Anything beyond
    /// that found at restart time still refuses it.
    pub fn force(self) -> Restart {
        Restart(Box::new(Consented::Shown(*self.plan, *self.stops)))
    }
}

/// A restart the user has agreed to. Only a plan's [`RestartApproval::Free`] and
/// [`WouldStop::force`] make one.
#[derive(Debug)]
pub struct Restart(Box<Consented>);

#[derive(Debug)]
enum Consented {
    /// Refused at restart time if it would stop anything by then.
    Clear(RestartPlan),
    /// Refused at restart time if it would stop more than was shown.
    Shown(RestartPlan, Stops),
}

impl Restart {
    pub fn plan(&self) -> &RestartPlan {
        match &*self.0 {
            Consented::Clear(plan) | Consented::Shown(plan, _) => plan,
        }
    }

    pub fn name(&self) -> SessionName {
        self.plan().name()
    }
}

#[derive(Debug)]
pub struct Restarted {
    pub end: End,
    pub label: &'static str,
}

/// What the session itself is doing, if the agent is in it.
fn session_interrupts(activity: Option<&Activity>) -> Option<Interrupts> {
    activity
        .and_then(|activity| activity.busy(SystemTime::now()))
        .map(Interrupts::Session)
}

fn survivor_interrupts(state: Option<SurvivorState>) -> Option<Interrupts> {
    (state == Some(SurvivorState::Busy)).then_some(Interrupts::Survivor)
}

/// Where an agent's harness process is looked for, which decides what
/// stopping it takes down.
#[derive(Debug, Clone, Copy)]
enum Seen {
    Live { stoppable: Option<SurvivorState> },
    Exited { process: Option<SurvivorState> },
}

impl Seen {
    fn of(was: &Was, lookup: &Lookup) -> Self {
        match was {
            Was::Live { .. } => Self::Live {
                stoppable: lookup.stoppable_state(),
            },
            Was::Down { .. } => Self::Exited {
                process: lookup.state(),
            },
        }
    }
}

/// What restarting would stop, the one rule for planning and for the
/// recheck. An exited agent with no process has no commands to lose.
fn would_stop(activity: Option<&Activity>, seen: Seen) -> Option<Stops> {
    let commands = || activity.and_then(Activity::commands).cloned();
    match seen {
        Seen::Live { stoppable } => Stops::of(
            session_interrupts(activity).or_else(|| survivor_interrupts(stoppable)),
            commands(),
        ),
        Seen::Exited { process: None } => None,
        Seen::Exited {
            process: Some(state),
        } => Stops::of(survivor_interrupts(Some(state)), commands()),
    }
}

fn plan_for(
    svc: Services<'_>,
    at: Startable,
    then: OnExited,
) -> Result<Planned<RestartApproval, OwnRestart>> {
    let name = at.name();
    let record = svc
        .store
        .record(&name)?
        .ok_or_else(|| Error::NoRecord(name.clone()))?;
    let harness = svc.harness(record.harness())?;
    let activity = svc.store.activities()?.remove(&name);
    let lookup = harness.survivor(
        at.cwd(),
        activity.as_ref().and_then(|a| a.transcript.as_ref()),
    )?;
    if matches!(lookup, Lookup::Elsewhere) {
        return Err(Error::OpenElsewhere {
            name,
            label: harness.label(),
        });
    }
    let was = if is_live(svc, &name)? {
        Was::Live {
            running: Running::new(name.clone()),
        }
    } else {
        Was::Down {
            survivor: lookup.state(),
            then,
        }
    };
    let stops = would_stop(activity.as_ref(), Seen::of(&was, &lookup));
    if matches!(was, Was::Live { .. }) && svc.own == Some(&name) {
        return Ok(Planned::Own(Own::new(name, OwnRestart { stops })));
    }
    let plan = RestartPlan {
        at,
        harness: record.harness().clone(),
        label: harness.label(),
        was,
    };
    Ok(Planned::Elsewhere(match stops {
        None => RestartApproval::Free(Restart(Box::new(Consented::Clear(plan)))),
        Some(stops) => RestartApproval::Needs(WouldStop {
            plan: Box::new(plan),
            stops: Box::new(stops),
        }),
    }))
}

/// What restarting `at` would do. An agent that already exited is started
/// again. Sessions `agent` didn't start have no record and are refused.
pub fn plan_restart(
    svc: Services<'_>,
    at: Startable,
) -> Result<Planned<RestartApproval, OwnRestart>> {
    plan_for(svc, at, OnExited::Start)
}

/// Stops the harness process a killed session left behind, then starts the
/// agent again. The order is fixed here: what would be interrupted is checked
/// before anything is touched, the session goes first so no client sees its
/// command end, and the process is gone before the launch is marked, so a late
/// report from it can't overwrite the mark.
pub fn restart(svc: Services<'_>, go: Restart) -> Result<Restarted> {
    restart_within(svc, go, Patience::SETTLE)
}

/// [`restart`], waiting `patience` for each thing that settles on its own.
pub fn restart_within(svc: Services<'_>, go: Restart, patience: Patience) -> Result<Restarted> {
    let (plan, shown) = match *go.0 {
        Consented::Clear(plan) => (plan, None),
        Consented::Shown(plan, stops) => (plan, Some(stops)),
    };
    let name = plan.at.name();
    let harness = svc.harness(&plan.harness)?;
    let activity = svc.store.activities()?.remove(&name);
    let transcript = activity.as_ref().and_then(|a| a.transcript.as_ref());
    let looked = || match harness.survivor(plan.at.cwd(), transcript)? {
        Lookup::Elsewhere => Err(Error::OpenElsewhere {
            name: name.clone(),
            label: plan.label,
        }),
        other => Ok(other),
    };
    let before = looked()?;
    if let Some(stops) = would_stop(activity.as_ref(), Seen::of(&plan.was, &before))
        .and_then(|fresh| fresh.beyond(shown.as_ref()))
    {
        return Err(Error::WouldStop {
            name,
            stops: Box::new(stops),
        });
    }
    if let Was::Live { running, .. } = &plan.was
        && is_live(svc, &name)?
    {
        svc.host.kill(running)?;
        if !patience.wait(|| Ok(!is_live(svc, &name)?))? {
            return Err(Error::Lingers(name));
        }
    }
    match clear(looked, patience)? {
        None => {}
        Some(Left::Stopped) => {
            return Err(Error::Survives {
                name,
                label: plan.label,
            });
        }
        Some(Left::NeverStoppable) => {
            return Err(Error::StillInTerminal {
                name,
                label: plan.label,
            });
        }
    }
    let end = plan.end();
    match end {
        End::Started => {
            start_then(svc, &plan.at, harness, None, || {
                svc.host.start_detached(&name)?;
                if patience.wait(|| is_live(svc, &name))? {
                    Ok(())
                } else {
                    Err(Error::NotStarted(name.clone()))
                }
            })?;
        }
        End::LeftExited => {}
    }
    Ok(Restarted {
        end,
        label: plan.label,
    })
}

fn is_live(svc: Services<'_>, name: &SessionName) -> Result<bool> {
    Ok(matches!(
        find_session(svc, name)?.map(|s| s.status),
        Some(SessionStatus::Live { .. })
    ))
}

/// What was left of a process after waiting for it to be gone.
enum Left {
    /// It was stopped and is still there.
    Stopped,
    /// It was alive the whole time but never became something to stop.
    NeverStoppable,
}

/// Stops what `lookup` finds, once, and waits for it to be gone. A process
/// that isn't stoppable yet is waited for. It is looked for again after the
/// stop because the harness's stop can report success and leave a process,
/// and a second worker of the same conversation can be behind the first.
/// What is left, if anything, is the answer.
fn clear(mut lookup: impl FnMut() -> Result<Lookup>, patience: Patience) -> Result<Option<Left>> {
    let mut left = Left::NeverStoppable;
    let gone = patience.wait(|| match lookup()? {
        Lookup::Gone => Ok(true),
        Lookup::Survivor(survivor) if matches!(left, Left::NeverStoppable) => {
            left = Left::Stopped;
            survivor.stop()?;
            Ok(false)
        }
        Lookup::Survivor(_) | Lookup::NotYet(_) | Lookup::Elsewhere => Ok(false),
    })?;
    Ok((!gone).then_some(left))
}

/// Whether a fleet restart may stop what the agents it found would stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopPolicy {
    Skip,
    Allow,
}

/// Every agent `agent` runs, as [`plan_restarts`] found them.
#[derive(Debug)]
pub struct Fleet {
    idle: Vec<Restart>,
    would_stop: Vec<WouldStop>,
    own: Option<Own<OwnRestart>>,
    unplanned: Vec<(SessionName, Error)>,
}

/// What a fleet approval decided.
#[derive(Debug)]
pub struct Approvals {
    pub restarts: Vec<Restart>,
    pub skipped: Vec<WouldStop>,
}

impl Fleet {
    pub fn idle(&self) -> &[Restart] {
        &self.idle
    }

    pub fn would_stop(&self) -> &[WouldStop] {
        &self.would_stop
    }

    /// The caller's own session, which a fleet restart leaves alone.
    pub fn own(&self) -> Option<&Own<OwnRestart>> {
        self.own.as_ref()
    }

    pub fn unplanned(&self) -> &[(SessionName, Error)] {
        &self.unplanned
    }

    pub fn is_empty(&self) -> bool {
        self.idle.is_empty() && self.would_stop.is_empty()
    }

    fn plans(&self) -> impl Iterator<Item = &RestartPlan> {
        self.idle
            .iter()
            .map(Restart::plan)
            .chain(self.would_stop.iter().map(WouldStop::plan))
    }

    pub fn starts(&self) -> usize {
        self.total() - self.stops()
    }

    pub fn stops(&self) -> usize {
        self.plans()
            .filter(|plan| plan.end() == End::LeftExited)
            .count()
    }

    pub fn total(&self) -> usize {
        self.idle.len() + self.would_stop.len()
    }

    /// What it would do, as `restart 5 · stop 2 exited · 3 busy`, each part
    /// left out when it is zero.
    pub fn summary(&self) -> String {
        self.summary_counting(0)
    }

    /// [`summary`](Self::summary) with `more` extra restarts, as when the
    /// caller's own session is restarted too.
    pub fn summary_counting(&self, more: usize) -> String {
        let counted = [
            (self.starts() + more, "restart {}"),
            (self.stops(), "stop {} exited"),
            (self.would_stop.len(), "{} busy"),
        ];
        counted
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, form)| form.replace("{}", &n.to_string()))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// [`summary`](Self::summary) as a title.
    pub fn title(&self) -> String {
        let summary = self.summary();
        let mut chars = summary.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }

    pub fn approve(self, policy: StopPolicy) -> Approvals {
        let mut restarts = self.idle;
        let mut skipped = Vec::new();
        for would_stop in self.would_stop {
            match policy {
                StopPolicy::Allow => restarts.push(would_stop.force()),
                StopPolicy::Skip => skipped.push(would_stop),
            }
        }
        Approvals { restarts, skipped }
    }
}

/// Every agent with a session, or an exited one whose harness left a process
/// running. Agents `agent` didn't start have no record and aren't listed.
pub fn plan_restarts(svc: Services<'_>) -> Result<Fleet> {
    let (found, _) = snapshot(svc)?;
    let mut fleet = Fleet {
        idle: Vec::new(),
        would_stop: Vec::new(),
        own: None,
        unplanned: Vec::new(),
    };
    for (_, entry) in found.entries() {
        let (Ok(at), Some(presence)) = (entry.startable(), entry.presence()) else {
            continue;
        };
        let then = match presence {
            Presence::Live => OnExited::Start,
            Presence::Exited => OnExited::Leave,
        };
        match plan_for(svc, at, then) {
            Ok(Planned::Own(own)) => fleet.own = Some(own),
            Ok(Planned::Elsewhere(approval)) if approval.plan().has_nothing_to_stop() => {}
            Err(Error::NoRecord(_)) => {}
            Ok(Planned::Elsewhere(RestartApproval::Free(restart))) => fleet.idle.push(restart),
            Ok(Planned::Elsewhere(RestartApproval::Needs(would_stop))) => {
                fleet.would_stop.push(would_stop);
            }
            Err(e) => fleet.unplanned.push((entry.name().clone(), e)),
        }
    }
    Ok(fleet)
}
